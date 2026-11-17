"use client";

import { memo, useEffect, useState, type FormEvent } from "react";
import {
  ChevronDown,
  Eye,
  EyeOff,
  HardDriveDownload,
  KeyRound,
  LoaderCircle,
  LogOut,
  Monitor,
  RotateCcw,
  Save,
  ScreenShare,
  Smartphone,
  Trash2,
  TriangleAlert,
  UserPlus,
  UserRound,
  Users,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { Accordion, RadioGroup, Slider, Switch } from "radix-ui";
import { toast } from "sonner";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import {
  createUser,
  deleteUser,
  getSessions,
  getSettings,
  getUsers,
  logoutOtherSessions,
  logoutSession,
  logoutUser,
  saveSettings,
  setUserPassword,
  type DeviceMode,
  type DisplayView,
  type SessionInfo,
  type Settings,
  type SettingsResponse,
  type UserInfo,
} from "@/lib/api";
import { formatAgo, formatTime } from "@/lib/format";
import { cn } from "@/lib/utils";

type Option<T extends string> = { value: T; label: string };

const SCALES = [1, 1.25, 1.5, 2, 2.5, 3];
const FPS = [15, 24, 30, 45, 60];
const CODECS: Option<Settings["webrtc_codec"]>[] = [
  { value: "auto", label: "Auto" },
  { value: "av1", label: "AV1" },
  { value: "h265", label: "H.265" },
  { value: "h264", label: "H.264" },
  { value: "vp9", label: "VP9" },
  { value: "vp8", label: "VP8" },
];
const QUALITIES: Option<Settings["webrtc_quality"]>[] = [
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "max", label: "Max" },
];
const SECTIONS = ["downloads", "display", "webrtc", "vnc", "users", "sessions"];
const OPEN_KEY = "chromine-settings-open";

function readOpen(): string[] {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(OPEN_KEY) ?? "null");
    if (Array.isArray(saved)) return saved.filter((s) => SECTIONS.includes(s));
  } catch {}
  return [];
}

const PILL =
  "h-7 flex-none cursor-pointer rounded-full px-3 text-[13px] font-medium text-white/55 transition-colors outline-none hover:text-white focus-visible:ring-2 focus-visible:ring-white/40 disabled:cursor-default disabled:opacity-40 disabled:hover:text-white/55 data-[state=checked]:bg-white data-[state=checked]:text-black data-[state=checked]:shadow-[0_1px_3px_rgba(0,0,0,0.5)]";

function Segmented<T extends string>({
  label,
  value,
  options,
  onChange,
  disabled,
  className,
}: {
  label: string;
  value: T;
  options: Option<T>[];
  onChange: (v: T) => void;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <RadioGroup.Root
      aria-label={label}
      value={value}
      disabled={disabled}
      onValueChange={(v) => v && v !== value && onChange(v as T)}
      orientation="horizontal"
      className={cn("inline-flex w-fit max-w-full flex-wrap gap-0.5 rounded-[18px] bg-white/[0.04] p-1 ring-1 ring-white/[0.07]", className)}
    >
      {options.map((o) => (
        <RadioGroup.Item key={o.value} value={o.value} className={PILL}>
          {o.label}
        </RadioGroup.Item>
      ))}
    </RadioGroup.Root>
  );
}

function Toggle({
  label,
  checked,
  onChange,
  disabled,
}: {
  label: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <Switch.Root
      aria-label={label}
      checked={checked}
      disabled={disabled}
      onCheckedChange={onChange}
      className="relative h-7 w-12 shrink-0 cursor-pointer rounded-full bg-white/[0.1] ring-1 ring-white/[0.1] transition-colors outline-none focus-visible:ring-2 focus-visible:ring-white/40 disabled:cursor-default disabled:opacity-40 data-[state=checked]:bg-white"
    >
      <Switch.Thumb className="block size-5 translate-x-1 rounded-full bg-white/70 shadow transition-transform data-[state=checked]:translate-x-6 data-[state=checked]:bg-black" />
    </Switch.Root>
  );
}

function Level({ label, value, onCommit }: { label: string; value: number; onCommit: (v: number) => void }) {
  const [v, setV] = useState(value);
  useEffect(() => setV(value), [value]);
  return (
    <div className="flex w-full items-center gap-3 md:w-64">
      <Slider.Root
        min={0}
        max={9}
        step={1}
        value={[v]}
        onValueChange={([n]) => setV(n)}
        onValueCommit={([n]) => n !== value && onCommit(n)}
        className="relative flex h-7 flex-1 touch-none items-center select-none"
      >
        <Slider.Track className="relative h-1.5 grow overflow-hidden rounded-full bg-white/[0.1]">
          <Slider.Range className="absolute h-full rounded-full bg-white" />
        </Slider.Track>
        <Slider.Thumb
          aria-label={label}
          className="block size-5 cursor-grab rounded-full bg-white shadow-[0_2px_8px_rgba(0,0,0,0.8)] outline-none focus-visible:ring-4 focus-visible:ring-white/25" />
      </Slider.Root>
      <span className="w-4 text-right text-sm font-medium text-white tabular-nums">{v}</span>
    </div>
  );
}

function Section({
  id,
  icon: Icon,
  title,
  summary,
  unsaved,
  children,
}: {
  id: string;
  icon: LucideIcon;
  title: string;
  summary: string;
  unsaved: number;
  children: React.ReactNode;
}) {
  return (
    <Accordion.Item
      value={id}
      className="overflow-hidden rounded-2xl bg-white/[0.015] ring-1 ring-white/[0.06] transition-colors data-[state=open]:bg-white/[0.025]"
    >
      <Accordion.Header>
        <Accordion.Trigger className="group flex w-full cursor-pointer items-start gap-3 px-4 py-4 text-left outline-none focus-visible:ring-2 focus-visible:ring-white/30 focus-visible:ring-inset sm:px-5">
          <span className="grid size-9 shrink-0 place-items-center rounded-xl bg-gradient-to-b from-white/[0.09] to-white/[0.02] ring-1 ring-white/10">
            <Icon className="size-[17px] text-white/70" aria-hidden />
          </span>
          <span className="min-w-0 flex-1 pt-0.5">
            <span className="flex items-center gap-2 text-sm font-semibold text-white">
              {title}
              {unsaved ? (
                <span className="rounded-full bg-warning/[0.12] px-2 py-px text-[11px] font-medium text-warning ring-1 ring-warning/25">
                  {unsaved} unsaved
                </span>
              ) : null}
            </span>
            <span className="mt-0.5 block text-xs leading-relaxed text-white/45">{summary}</span>
          </span>
          <ChevronDown
            className="mt-2.5 size-4 shrink-0 text-white/40 transition-transform duration-200 group-data-[state=open]:rotate-180 motion-reduce:transition-none"
            aria-hidden
          />
        </Accordion.Trigger>
      </Accordion.Header>
      <Accordion.Content className="overflow-hidden data-[state=closed]:animate-accordion-up data-[state=open]:animate-accordion-down motion-reduce:animate-none">
        <div className="divide-y divide-white/[0.05] border-t border-white/[0.05]">
          {children}
        </div>
      </Accordion.Content>
    </Accordion.Item>
  );
}

function Row({
  label,
  hint,
  changed,
  unsaved,
  inline,
  children,
}: {
  label: string;
  hint?: React.ReactNode;
  changed?: boolean;
  unsaved?: boolean;
  inline?: boolean;
  children: React.ReactNode;
}) {
  return (
    <div
      className={cn(
        "grid gap-3 px-4 py-4 sm:px-5 md:grid-cols-[minmax(0,1fr)_auto] md:items-center md:gap-8",
        inline && "grid-cols-[minmax(0,1fr)_auto] items-center gap-4",
      )}
    >
      <div className="min-w-0">
        <p className="flex items-center gap-2 text-sm font-medium text-white">
          {label}
          {unsaved ? (
            <span className="size-1.5 rounded-full bg-warning" title="Not saved yet" aria-label="not saved" />
          ) : changed ? (
            <span className="size-1.5 rounded-full bg-info" title="Changed from the default" aria-label="changed" />
          ) : null}
        </p>
        {hint ? <p className="mt-1 text-xs leading-relaxed text-white/45">{hint}</p> : null}
      </div>
      <div className="min-w-0">{children}</div>
    </div>
  );
}

function deviceName(ua: string | null) {
  if (!ua) return "Unknown device";
  const os = /Android/i.test(ua)
    ? "Android"
    : /iPhone|iPad|iPod/i.test(ua)
      ? "iOS"
      : /CrOS/.test(ua)
        ? "ChromeOS"
        : /Windows/i.test(ua)
          ? "Windows"
          : /Macintosh|Mac OS X/i.test(ua)
            ? "macOS"
            : /Linux/i.test(ua)
              ? "Linux"
              : null;
  const browser = /Edg(A|iOS)?\//.test(ua)
    ? "Edge"
    : /OPR\/|Opera/.test(ua)
      ? "Opera"
      : /SamsungBrowser/.test(ua)
        ? "Samsung Internet"
        : /Firefox\/|FxiOS/.test(ua)
          ? "Firefox"
          : /CriOS|Chrome\//.test(ua)
            ? "Chrome"
            : /Safari\//.test(ua)
              ? "Safari"
              : null;
  if (browser && os) return `${browser} on ${os}`;
  return browser ?? os ?? "Unknown browser";
}

function SessionItem({
  session: s,
  now,
  busy,
  onLogout,
}: {
  session: SessionInfo;
  now: number;
  busy: boolean;
  onLogout: () => void;
}) {
  const Icon = /Mobi|Android|iPhone|iPad/i.test(s.user_agent ?? "") ? Smartphone : Monitor;
  const details = [s.ip, `signed in ${formatTime(s.created_at)}`, s.current ? "active now" : `active ${formatAgo(s.last_seen_at, now)}`];
  return (
    <div className="flex items-center gap-3 px-4 py-3.5 sm:px-5">
      <span className="grid size-9 shrink-0 place-items-center rounded-xl bg-white/[0.04] text-white/60 ring-1 ring-white/[0.07]">
        <Icon className="size-4" aria-hidden />
      </span>
      <div className="min-w-0 flex-1">
        <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm font-medium text-white" title={s.user_agent ?? undefined}>
          {deviceName(s.user_agent)}
          {s.current ? (
            <span className="rounded-full bg-success/[0.12] px-2 py-px text-[11px] font-medium text-success ring-1 ring-success/25">
              This device
            </span>
          ) : null}
        </p>
        <p className="mt-0.5 text-xs leading-relaxed break-words text-white/45 tabular-nums">{details.filter(Boolean).join(" · ")}</p>
      </div>
      {s.current ? null : (
        <Button
          variant="ghost"
          size="sm"
          disabled={busy}
          onClick={onLogout}
          className="h-8 shrink-0 rounded-full bg-white/[0.04] px-3 text-[13px] text-white/75 ring-1 ring-white/[0.08] hover:bg-destructive/15 hover:text-destructive dark:hover:bg-destructive/15"
        >
          {busy ? <LoaderCircle className="animate-spin" /> : <LogOut />}
          Log out
        </Button>
      )}
    </div>
  );
}

const FIELD =
  "h-9 w-full min-w-0 rounded-full bg-white/[0.04] px-3.5 text-base text-white ring-1 ring-white/[0.08] outline-none placeholder:text-white/30 focus:ring-white/30 sm:text-[13px]";
const ACTION =
  "h-8 shrink-0 rounded-full bg-white/[0.04] px-3 text-[13px] text-white/75 ring-1 ring-white/[0.08] hover:bg-white/[0.09] hover:text-white dark:hover:bg-white/[0.09]";
const DANGER =
  "h-8 shrink-0 rounded-full bg-white/[0.04] px-3 text-[13px] text-white/75 ring-1 ring-white/[0.08] hover:bg-destructive/15 hover:text-destructive dark:hover:bg-destructive/15";
const PRIMARY =
  "h-9 shrink-0 rounded-full bg-white px-4 text-black hover:bg-white/85 disabled:bg-white/[0.06] disabled:text-white/35";
const DIALOG = "rounded-2xl border-0 bg-[#0b0b0b] shadow-[0_30px_120px_rgba(0,0,0,1)] ring-1 ring-white/10";
const DIALOG_CANCEL =
  "rounded-full border-0 bg-white/[0.05] ring-1 ring-white/10 hover:bg-white/10 dark:bg-white/[0.05] dark:hover:bg-white/10";

function SessionsSection({ active, open }: { active: boolean; open: boolean }) {
  const [sessions, setSessions] = useState<SessionInfo[] | null>(null);
  const [endingSession, setEndingSession] = useState<string | null>(null);

  const loadSessions = () => {
    getSessions()
      .then((r) => setSessions(r.sessions))
      .catch(() => {});
  };

  useEffect(() => {
    if (active) loadSessions();
  }, [active, open]);

  const endSession = async (s: SessionInfo) => {
    setEndingSession(s.id);
    try {
      await logoutSession(s.id);
      toast.success("Signed out", { description: deviceName(s.user_agent) });
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "Could not sign out that session");
    } finally {
      setEndingSession(null);
      loadSessions();
    }
  };

  const endOtherSessions = async () => {
    try {
      const r = await logoutOtherSessions();
      toast.success(r.logged_out === 1 ? "Signed out 1 other session" : `Signed out ${r.logged_out} other sessions`);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "Could not sign out the other sessions");
    } finally {
      loadSessions();
    }
  };

  const sessionList = [...(sessions ?? [])].sort(
    (a, b) => Number(b.current) - Number(a.current) || b.last_seen_at - a.last_seen_at,
  );
  const others = sessionList.filter((s) => !s.current).length;
  const sessionSummary =
    sessions === null
      ? "Devices signed in to this dashboard"
      : others === 0
        ? "Only this device is signed in"
        : `${sessionList.length} signed in · ${others} other device${others === 1 ? "" : "s"}`;
  const now = Date.now();

  return (
    <Section id="sessions" icon={KeyRound} title="Sessions" summary={sessionSummary} unsaved={0}>
      {sessions === null ? (
        <p className="flex items-center gap-2 px-4 py-4 text-xs text-white/40 sm:px-5">
          <LoaderCircle className="size-3.5 animate-spin" aria-hidden /> Loading sessions…
        </p>
      ) : (
        sessionList.map((s) => (
          <SessionItem
            key={s.id}
            session={s}
            now={now}
            busy={endingSession === s.id}
            onLogout={() => endSession(s)}
          />
        ))
      )}
      {others > 0 ? (
        <div className="flex justify-end px-4 py-3 sm:px-5">
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button
                variant="ghost"
                size="sm"
                className="h-9 rounded-full bg-white/[0.04] px-3.5 text-[13px] text-white/75 ring-1 ring-white/[0.08] hover:bg-destructive/15 hover:text-destructive dark:hover:bg-destructive/15"
              >
                <LogOut />
                Log out all other sessions
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent className={DIALOG}>
              <AlertDialogHeader>
                <AlertDialogTitle>Log out every other session?</AlertDialogTitle>
                <AlertDialogDescription className="text-white/55">
                  {others === 1 ? "1 other device" : `${others} other devices`} will have to sign in again. This device stays signed in.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel className={DIALOG_CANCEL}>Cancel</AlertDialogCancel>
                <AlertDialogAction className="rounded-full bg-white text-black hover:bg-white/85" onClick={endOtherSessions}>
                  Log out others
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        </div>
      ) : null}
    </Section>
  );
}

function PasswordField({
  value,
  onChange,
  label,
  autoFocus,
}: {
  value: string;
  onChange: (v: string) => void;
  label: string;
  autoFocus?: boolean;
}) {
  const [shown, setShown] = useState(false);
  return (
    <div className="relative min-w-0 sm:flex-1">
      <input
        type={shown ? "text" : "password"}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={label}
        aria-label={label}
        autoComplete="new-password"
        autoCapitalize="off"
        spellCheck={false}
        autoFocus={autoFocus}
        className={cn(FIELD, "pr-10")}
      />
      <button
        type="button"
        onClick={() => setShown(!shown)}
        aria-label={shown ? "Hide password" : "Show password"}
        className="absolute top-1/2 right-1 grid size-7 -translate-y-1/2 cursor-pointer place-items-center rounded-full text-white/45 outline-none hover:text-white focus-visible:ring-2 focus-visible:ring-white/40"
      >
        {shown ? <EyeOff className="size-3.5" aria-hidden /> : <Eye className="size-3.5" aria-hidden />}
      </button>
    </div>
  );
}

function UserItem({
  user: u,
  now,
  busy,
  editing,
  minPassword,
  onEdit,
  onPassword,
  onLogout,
  onDelete,
}: {
  user: UserInfo;
  now: number;
  busy: boolean;
  editing: boolean;
  minPassword: number;
  onEdit: (on: boolean) => void;
  onPassword: (password: string) => Promise<boolean>;
  onLogout: () => void;
  onDelete: () => void;
}) {
  const [password, setPassword] = useState("");
  const online = u.sessions > 0;
  const details = [
    online ? (u.sessions === 1 ? "signed in on 1 device" : `signed in on ${u.sessions} devices`) : "signed out",
    u.last_seen_at ? `active ${formatAgo(u.last_seen_at, now)}` : "never signed in",
    `created ${formatTime(u.created_at)}`,
  ];
  const close = () => {
    setPassword("");
    onEdit(false);
  };
  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (await onPassword(password)) close();
  };
  return (
    <div className="px-4 py-3.5 sm:px-5">
      <div className="flex flex-wrap items-center gap-3">
        <span className="grid size-9 shrink-0 place-items-center rounded-xl bg-white/[0.04] text-white/60 ring-1 ring-white/[0.07]">
          <UserRound className="size-4" aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <p className="flex items-center gap-2 text-sm font-medium text-white">
            <span className="truncate">{u.username}</span>
            {online ? (
              <span className="shrink-0 rounded-full bg-success/[0.12] px-2 py-px text-[11px] font-medium text-success ring-1 ring-success/25">
                Signed in
              </span>
            ) : null}
          </p>
          <p className="mt-0.5 text-xs leading-relaxed break-words text-white/45 tabular-nums">{details.join(" · ")}</p>
        </div>
        <div className="flex w-full items-center justify-end gap-1.5 sm:w-auto">
          <Button variant="ghost" size="sm" disabled={busy} onClick={() => (editing ? close() : onEdit(true))} className={ACTION}>
            <KeyRound />
            Password
          </Button>
          {online ? (
            <Button variant="ghost" size="sm" disabled={busy} onClick={onLogout} className={DANGER}>
              <LogOut />
              Sign out
            </Button>
          ) : null}
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button variant="ghost" size="sm" disabled={busy} className={DANGER}>
                <Trash2 />
                Delete
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent className={DIALOG}>
              <AlertDialogHeader>
                <AlertDialogTitle>Delete {u.username}?</AlertDialogTitle>
                <AlertDialogDescription className="text-white/55">
                  They are signed out, and their Chrome profile, saved logins, VS Code files, downloads, transfer list and
                  settings are removed from this server. This can&apos;t be undone.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel className={DIALOG_CANCEL}>Cancel</AlertDialogCancel>
                <AlertDialogAction className="rounded-full bg-destructive text-white hover:bg-destructive/85" onClick={onDelete}>
                  Delete user
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        </div>
      </div>
      {editing ? (
        <form onSubmit={save} className="mt-3 grid gap-2 sm:pl-12">
          <div className="flex flex-col gap-2 sm:flex-row">
            <PasswordField value={password} onChange={setPassword} label="New password" autoFocus />
            <div className="flex justify-end gap-1.5">
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={close}
                className="h-9 rounded-full px-3.5 text-[13px] text-white/60 hover:bg-white/[0.07] hover:text-white dark:hover:bg-white/[0.07]"
              >
                Cancel
              </Button>
              <Button type="submit" size="sm" disabled={busy || password.length < minPassword} className={PRIMARY}>
                {busy ? <LoaderCircle className="animate-spin" /> : <Save />}
                Save
              </Button>
            </div>
          </div>
          <p className="text-xs leading-relaxed text-white/45">
            At least {minPassword} characters.
            {online ? ` ${u.username} is signed out and must sign in with the new password.` : ""}
          </p>
        </form>
      ) : null}
    </div>
  );
}

function UsersSection({ active, open }: { active: boolean; open: boolean }) {
  const [users, setUsers] = useState<UserInfo[] | null>(null);
  const [minPassword, setMinPassword] = useState(8);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [creating, setCreating] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);

  const load = () => {
    getUsers()
      .then((r) => {
        setUsers(r.users);
        setMinPassword(r.min_password);
      })
      .catch(() => {});
  };

  useEffect(() => {
    if (active) load();
  }, [active, open]);

  useEffect(() => {
    if (!active || !open) return;
    const t = setInterval(load, 10_000);
    return () => clearInterval(t);
  }, [active, open]);

  const create = async (e: FormEvent) => {
    e.preventDefault();
    if (creating) return;
    setCreating(true);
    try {
      const r = await createUser(username, password);
      toast.success(`Created ${r.username}`, { description: "They can sign in now with the password you set." });
      setUsername("");
      setPassword("");
      load();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "Could not create the user");
    } finally {
      setCreating(false);
    }
  };

  const act = async (u: UserInfo, run: () => Promise<unknown>, done: string, description?: string) => {
    setBusy(u.id);
    try {
      await run();
      toast.success(done, description ? { description } : undefined);
      return true;
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "Something went wrong");
      return false;
    } finally {
      setBusy(null);
      load();
    }
  };

  const signedIn = users?.filter((u) => u.sessions > 0).length ?? 0;
  const summary =
    users === null
      ? "Logins for other people, each with their own Chrome"
      : users.length === 0
        ? "No other users yet"
        : `${users.length} user${users.length === 1 ? "" : "s"} · ${signedIn} signed in`;
  const now = Date.now();

  return (
    <Section id="users" icon={Users} title="Users" summary={summary} unsaved={0}>
      <form onSubmit={create} className="grid gap-3 px-4 py-4 sm:px-5">
        <div>
          <p className="text-sm font-medium text-white">New user</p>
          <p className="mt-1 text-xs leading-relaxed text-white/45">
            Each user gets their own Chrome, VS Code, downloads, transfers and settings. These start when they sign in and
            close when they sign out. Passwords need at least {minPassword} characters.
          </p>
        </div>
        <div className="flex flex-col gap-2 sm:flex-row">
          <input
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            placeholder="Username"
            aria-label="Username"
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            className={cn(FIELD, "sm:flex-1")}
          />
          <PasswordField value={password} onChange={setPassword} label="Password" />
          <Button
            type="submit"
            size="sm"
            disabled={creating || username.trim().length < 3 || password.length < minPassword}
            className={PRIMARY}
          >
            {creating ? <LoaderCircle className="animate-spin" /> : <UserPlus />}
            Create user
          </Button>
        </div>
      </form>
      {users === null ? (
        <p className="flex items-center gap-2 px-4 py-4 text-xs text-white/40 sm:px-5">
          <LoaderCircle className="size-3.5 animate-spin" aria-hidden /> Loading users…
        </p>
      ) : (
        users.map((u) => (
          <UserItem
            key={u.id}
            user={u}
            now={now}
            busy={busy === u.id}
            editing={editing === u.id}
            minPassword={minPassword}
            onEdit={(on) => setEditing(on ? u.id : null)}
            onPassword={(p) =>
              act(
                u,
                () => setUserPassword(u.id, p),
                "Password changed",
                u.sessions > 0 ? `${u.username} was signed out.` : undefined,
              )
            }
            onLogout={() =>
              act(u, () => logoutUser(u.id), `Signed out ${u.username}`, "Their Chrome and VS Code close in a few seconds.")
            }
            onDelete={() => act(u, () => deleteUser(u.id), `Deleted ${u.username}`)}
          />
        ))
      )}
    </Section>
  );
}

function modeName(m: string) {
  return m === "vnc" ? "VNC" : "WebRTC";
}

function downloadSummary(v: Settings) {
  if (!v.filecloud) return "New downloads stay on the server. Nothing is uploaded.";
  const after = v.keep_local ? "The copy on the server is kept." : "The copy on the server is then deleted.";
  return `New downloads are uploaded to FileCloud24. ${after}`;
}

function saveNote(patch: Partial<Settings>) {
  const notes: string[] = [];
  if (patch.display_scale !== undefined) notes.push("The remote Chrome is restarting. Your tabs will reopen.");
  if (patch.webrtc_codec) notes.push("WebRTC viewers reconnect with the new codec.");
  return notes.join(" ") || undefined;
}

function normalized(s: Settings): Settings {
  return { ...s, webrtc_host: s.webrtc_host.trim() };
}

function unsavedSettings(saved: Settings, draft: Settings): Partial<Settings> {
  const next = normalized(draft);
  const out: Partial<Settings> = {};
  for (const k of Object.keys(next) as (keyof Settings)[]) {
    if (next[k] !== saved[k]) Object.assign(out, { [k]: next[k] });
  }
  return out;
}

function display(v: Settings): DisplayView {
  return {
    stream_mode: v.stream_mode,
    display_scale: v.display_scale,
    vnc_quality: v.vnc_quality,
    vnc_compression: v.vnc_compression,
  };
}

export const SettingsPanel = memo(function SettingsPanel({
  admin,
  active,
  onDisplay,
  deviceMode,
  onDeviceMode,
}: {
  admin: boolean;
  active: boolean;
  onDisplay: (d: DisplayView) => void;
  deviceMode: DeviceMode;
  onDeviceMode: (m: DeviceMode) => void;
}) {
  const [data, setData] = useState<SettingsResponse | null>(null);
  const [draft, setDraft] = useState<Settings | null>(null);
  const [device, setDevice] = useState<DeviceMode>(deviceMode);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [open, setOpen] = useState<string[]>(readOpen);

  const patch = data && draft ? unsavedSettings(data.values, draft) : {};
  const unsaved = Object.keys(patch).length + (device !== deviceMode ? 1 : 0);

  const load = () => {
    setError(null);
    getSettings()
      .then((r) => {
        setData(r);
        setDraft(r.values);
        setDevice(deviceMode);
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  };

  useEffect(() => {
    if (active && !unsaved) load();
  }, [active]);

  useEffect(() => {
    if (!unsaved) return;
    const warn = (e: BeforeUnloadEvent) => e.preventDefault();
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [unsaved > 0]);

  const remember = (sections: string[]) => {
    setOpen(sections);
    try {
      localStorage.setItem(OPEN_KEY, JSON.stringify(sections));
    } catch {}
  };

  if (!data || !draft) {
    return (
      <div className="flex h-64 flex-col items-center justify-center gap-3 text-sm text-white/40">
        {error ? (
          <>
            <p className="text-destructive">{error}</p>
            <Button variant="ghost" size="sm" className="rounded-full" onClick={load}>
              Try again
            </Button>
          </>
        ) : (
          <span className="flex items-center">
            <LoaderCircle className="mr-2 size-4 animate-spin" aria-hidden /> Loading…
          </span>
        )}
      </div>
    );
  }

  const v = draft;
  const next = normalized(v);
  const d = data.defaults;
  const changed = (k: keyof Settings) => next[k] !== d[k];
  const pending = (k: keyof Settings) => k in patch;
  const set = (p: Partial<Settings>) => setDraft({ ...v, ...p });

  const save = async () => {
    if (!unsaved || saving) return;
    setSaving(true);
    try {
      if (Object.keys(patch).length) {
        const r = await saveSettings(patch);
        setData(r);
        setDraft(r.values);
        onDisplay(display(r.values));
      }
      if (device !== deviceMode) onDeviceMode(device);
      const description = saveNote(patch);
      toast.success("Settings saved", description ? { description } : undefined);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "Could not save the settings");
    } finally {
      setSaving(false);
    }
  };

  const discard = () => {
    setDraft(data.values);
    setDevice(deviceMode);
  };

  const count = (keys: (keyof Settings)[]) => keys.filter(pending).length;
  const label = <T extends string>(options: Option<T>[], value: T) => options.find((o) => o.value === value)?.label ?? value;

  const scales = SCALES.includes(v.display_scale) ? SCALES : [...SCALES, v.display_scale].sort((a, b) => a - b);

  return (
    <div className="mx-auto grid w-full max-w-3xl grid-cols-1 gap-4 px-3 pt-4 pb-24 sm:gap-5 sm:px-5 sm:pt-5">
      <Accordion.Root type="multiple" value={open} onValueChange={remember} className="grid grid-cols-1 gap-3 sm:gap-4">
        <Section
          id="downloads"
          icon={HardDriveDownload}
          title="Downloads"
          summary={downloadSummary(v)}
          unsaved={count(["filecloud", "keep_local"])}
        >
          <Row
            label="Upload to FileCloud24"
            hint="Upload each finished download to filecloud24.cc."
            changed={changed("filecloud")}
            unsaved={pending("filecloud")}
            inline
          >
            <Toggle label="Upload to FileCloud24" checked={v.filecloud} onChange={(filecloud) => set({ filecloud })} />
          </Row>
          <Row
            label="Keep a copy on the server"
            changed={changed("keep_local")}
            unsaved={pending("keep_local")}
            inline
            hint="After a file is uploaded, keep it in the downloads folder instead of deleting it."
          >
            <Toggle
              label="Keep a copy on the server"
              checked={v.keep_local}
              disabled={!v.filecloud}
              onChange={(keep_local) => set({ keep_local })}
            />
          </Row>
        </Section>

        <Section
          id="display"
          icon={Monitor}
          title="Display"
          summary={`${modeName(v.stream_mode)} stream · ${v.display_scale}× scale${device === "default" ? "" : ` · this device uses ${modeName(device)}`}`}
          unsaved={count(["stream_mode", "display_scale"]) + (device !== deviceMode ? 1 : 0)}
        >
          <Row label="Stream mode" hint="The default for every device." changed={changed("stream_mode")} unsaved={pending("stream_mode")}>
            <Segmented
              label="Stream mode"
              value={v.stream_mode}
              onChange={(stream_mode) => set({ stream_mode })}
              options={[
                { value: "vnc", label: "VNC" },
                { value: "webrtc", label: "WebRTC" },
              ]}
            />
          </Row>
          <Row
            label="This device"
            hint={`Overrides the stream mode in this browser only. Now using ${modeName(deviceMode === "default" ? data.values.stream_mode : deviceMode)}.`}
            changed={device !== "default"}
            unsaved={device !== deviceMode}
          >
            <Segmented
              label="This device"
              value={device}
              onChange={setDevice}
              options={[
                { value: "default", label: "Default" },
                { value: "vnc", label: "VNC" },
                { value: "webrtc", label: "WebRTC" },
              ]}
            />
          </Row>
          <Row
            label="Display scale"
            hint="Sharper text costs more CPU and bandwidth. Saving a new scale restarts the remote Chrome; your tabs reopen."
            changed={changed("display_scale")}
            unsaved={pending("display_scale")}
          >
            <Segmented
              label="Display scale"
              value={String(v.display_scale)}
              onChange={(s) => set({ display_scale: Number(s) })}
              options={scales.map((s) => ({ value: String(s), label: `${s}×` }))}
            />
          </Row>
        </Section>

        <Section
          id="webrtc"
          icon={Zap}
          title="WebRTC"
          summary={
            data.webrtc_ready
              ? `${label(CODECS, v.webrtc_codec)} codec · ${v.webrtc_fps} fps · ${label(QUALITIES, v.webrtc_quality)} quality`
              : "Unavailable on this server"
          }
          unsaved={count(["webrtc_codec", "webrtc_fps", "webrtc_quality", "webrtc_host"])}
        >
          {!data.webrtc_ready ? (
            <p className="flex items-start gap-2 bg-warning/[0.06] px-4 py-3 text-xs leading-relaxed text-warning sm:px-5">
              <TriangleAlert className="mt-px size-3.5 shrink-0" aria-hidden />
              WebRTC could not start on this server (its UDP port did not open). Check the worker log.
            </p>
          ) : null}
          <Row
            label="Codec"
            hint="Auto uses AV1, then H.264, VP9, VP8. A device that can't decode your pick falls back in that order. H.265 needs Safari or hardware decoding. Viewers reconnect."
            changed={changed("webrtc_codec")}
            unsaved={pending("webrtc_codec")}
          >
            <Segmented
              label="Codec"
              className="grid grid-cols-3 sm:inline-flex"
              value={v.webrtc_codec}
              onChange={(webrtc_codec) => set({ webrtc_codec })}
              options={CODECS}
            />
          </Row>
          <Row
            label="Frame rate"
            hint="Highest frame rate while the screen is moving. More frames cost more CPU."
            changed={changed("webrtc_fps")}
            unsaved={pending("webrtc_fps")}
          >
            <Segmented
              label="Frame rate"
              value={String(v.webrtc_fps)}
              onChange={(f) => set({ webrtc_fps: Number(f) })}
              options={(FPS.includes(v.webrtc_fps) ? FPS : [...FPS, v.webrtc_fps].sort((a, b) => a - b)).map((f) => ({
                value: String(f),
                label: `${f}`,
              }))}
            />
          </Row>
          <Row
            label="Video quality"
            hint="Higher quality looks sharper and uses more bandwidth."
            changed={changed("webrtc_quality")}
            unsaved={pending("webrtc_quality")}
          >
            <Segmented
              label="Video quality"
              value={v.webrtc_quality}
              onChange={(webrtc_quality) => set({ webrtc_quality })}
              options={QUALITIES}
            />
          </Row>
          {admin ? (
            <Row
              label="Server address"
              hint="The IP your devices reach the server on. Leave empty to use the address in the browser bar. Applies to the next connection."
              changed={changed("webrtc_host")}
              unsaved={pending("webrtc_host")}
            >
              <form
                className="w-full md:w-72"
                onSubmit={(e) => {
                  e.preventDefault();
                  save();
                }}
              >
                <input
                  value={v.webrtc_host}
                  onChange={(e) => set({ webrtc_host: e.target.value })}
                  placeholder="Automatic"
                  aria-label="Server address"
                  spellCheck={false}
                  autoCapitalize="off"
                  autoComplete="off"
                  enterKeyHint="done"
                  className="h-9 w-full min-w-0 rounded-full bg-white/[0.04] px-3.5 font-mono text-base text-white ring-1 ring-white/[0.08] outline-none placeholder:font-sans placeholder:text-white/30 focus:ring-white/30 sm:text-[13px]"
                />
              </form>
            </Row>
          ) : null}
        </Section>

        <Section
          id="vnc"
          icon={ScreenShare}
          title="VNC"
          summary={`Image quality ${v.vnc_quality} · compression ${v.vnc_compression}`}
          unsaved={count(["vnc_quality", "vnc_compression"])}
        >
          <Row
            label="Image quality"
            hint="0 is blurry and light, 9 is the sharpest."
            changed={changed("vnc_quality")}
            unsaved={pending("vnc_quality")}
          >
            <Level label="Image quality" value={v.vnc_quality} onCommit={(vnc_quality) => set({ vnc_quality })} />
          </Row>
          <Row
            label="Compression"
            hint="Higher squeezes harder: less data, more CPU."
            changed={changed("vnc_compression")}
            unsaved={pending("vnc_compression")}
          >
            <Level label="Compression" value={v.vnc_compression} onCommit={(vnc_compression) => set({ vnc_compression })} />
          </Row>
        </Section>
        {admin ? <UsersSection active={active} open={open.includes("users")} /> : null}
        <SessionsSection active={active} open={open.includes("sessions")} />
      </Accordion.Root>

      <div className="flex px-1">
        <Button
          variant="ghost"
          size="sm"
          disabled={saving}
          onClick={() => setDraft(d)}
          className="h-9 rounded-full bg-white/[0.04] px-3.5 text-[13px] text-white/75 ring-1 ring-white/[0.08] hover:bg-white/[0.09] hover:text-white dark:hover:bg-white/[0.09]"
        >
          <RotateCcw />
          Reset to defaults
        </Button>
      </div>

      <div className="sticky bottom-4 z-30 ml-14 lg:ml-0">
        <div className="flex items-center justify-between gap-3 rounded-full bg-[#0b0b0b] py-1.5 pr-1.5 pl-4 shadow-[0_20px_60px_rgba(0,0,0,0.95)] ring-1 ring-white/10">
          <p className={cn("min-w-0 truncate text-[13px]", unsaved ? "text-warning" : "text-white/45")} aria-live="polite">
            {unsaved ? `${unsaved} unsaved change${unsaved === 1 ? "" : "s"}` : "All changes saved"}
          </p>
          <div className="flex shrink-0 items-center gap-1">
            {unsaved ? (
              <Button
                variant="ghost"
                size="sm"
                disabled={saving}
                onClick={discard}
                className="h-9 rounded-full px-3.5 text-[13px] text-white/60 hover:bg-white/[0.07] hover:text-white dark:hover:bg-white/[0.07]"
              >
                Discard
              </Button>
            ) : null}
            <Button
              size="sm"
              disabled={!unsaved || saving}
              onClick={save}
              className="h-9 rounded-full bg-white px-4 text-black hover:bg-white/85 disabled:bg-white/[0.06] disabled:text-white/35"
            >
              {saving ? <LoaderCircle className="animate-spin" /> : <Save />}
              Save
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
});
