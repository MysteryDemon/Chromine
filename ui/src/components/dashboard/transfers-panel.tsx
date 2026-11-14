"use client";

import { memo, useRef, useState } from "react";
import {
  Activity,
  Check,
  ChevronLeft,
  ChevronRight,
  CircleCheck,
  CloudUpload,
  Copy,
  ExternalLink,
  File,
  FileDown,
  FolderSearch,
  HardDrive,
  Inbox,
  LoaderCircle,
  RotateCcw,
  Trash2,
  TriangleAlert,
  X,
} from "lucide-react";
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
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { cancelJob, clearHistory, importOrphan, retryJob, safeHref, type DashboardState, type Job } from "@/lib/api";
import { formatBytes, formatTime, formatUntil, percent } from "@/lib/format";
import { cn } from "@/lib/utils";
import { CleanupBadge, UploadBadge } from "./state-badge";

const COLUMNS =
  "lg:grid-cols-[minmax(0,1.7fr)_minmax(0,1.15fr)_minmax(0,0.95fr)_minmax(0,1.25fr)_6.5rem]";

function needsAttention(j: Job) {
  return j.upload_state === "failed" || j.cleanup_state === "failed" || j.file_state === "missing";
}

function isDone(j: Job) {
  return (
    (j.upload_state === "uploaded" || j.upload_state === "skipped") &&
    (j.cleanup_state === "deleted" || j.cleanup_state === "retained")
  );
}

function ErrorText({ children }: { children: string | null }) {
  if (!children) return null;
  return <p className="mt-1.5 text-xs leading-snug break-words text-destructive/90">{children}</p>;
}

function Meta({ children }: { children: React.ReactNode }) {
  return <p className="mt-1.5 text-xs text-white/40 tabular-nums">{children}</p>;
}

function Bar({ value }: { value: number }) {
  return (
    <div
      role="progressbar"
      aria-label="Upload progress"
      aria-valuenow={value}
      aria-valuemin={0}
      aria-valuemax={100}
      className="h-1 w-full max-w-44 overflow-hidden rounded-full bg-white/[0.07]"
    >
      <div
        className="h-full rounded-full bg-gradient-to-r from-info/60 to-info shadow-[0_0_12px_rgba(106,168,255,0.55)] transition-[width] duration-700"
        style={{ width: `${value}%` }}
      />
    </div>
  );
}

function Stage({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex min-w-0 items-start gap-3 lg:block">
      <span className="w-20 shrink-0 pt-[5px] text-[11px] font-medium tracking-[0.08em] text-white/35 uppercase lg:hidden">
        {label}
      </span>
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

function UploadStage({ job, now }: { job: Job; now: number }) {
  const showProgress =
    job.upload_state === "uploading" ||
    job.upload_state === "finalizing" ||
    (job.progress_bytes > 0 && job.upload_state !== "uploaded" && job.upload_state !== "cancelled");
  const pct = percent(job.progress_bytes, job.size_bytes);
  return (
    <Stage label="Upload">
      <UploadBadge state={job.upload_state} />
      {showProgress ? (
        <div className="mt-2.5">
          <Bar value={pct} />
          <Meta>
            {formatBytes(job.progress_bytes)} of {formatBytes(job.size_bytes)} · {pct}%
          </Meta>
        </div>
      ) : null}
      {job.upload_state === "retry_wait" ? <Meta>Next try {formatUntil(job.upload_next_at, now)}</Meta> : null}
      {job.upload_attempts > 0 && job.upload_state !== "uploaded" ? <Meta>{job.upload_attempts} failed attempt(s)</Meta> : null}
      {job.upload_state !== "uploaded" || job.file_state === "missing" ? <ErrorText>{job.upload_error}</ErrorText> : null}
    </Stage>
  );
}

function CleanupStage({ job }: { job: Job }) {
  return (
    <Stage label="Local file">
      <CleanupBadge state={job.cleanup_state} missing={job.file_state === "missing"} />
      <ErrorText>{job.cleanup_error}</ErrorText>
    </Stage>
  );
}

function LinkChip({ href }: { href: string }) {
  const [copied, setCopied] = useState(false);
  const display = href.replace(/^https?:\/\//, "");
  return (
    <div className="flex w-fit max-w-full min-w-0 items-center gap-0.5 rounded-full bg-white/[0.04] py-0.5 pr-0.5 pl-3 ring-1 ring-white/[0.07]">
      <a
        href={href}
        target="_blank"
        rel="noopener noreferrer"
        className="min-w-0 truncate font-mono text-xs text-info underline-offset-4 hover:underline"
        title={href}
      >
        {display}
      </a>
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            className="size-7 shrink-0 rounded-full text-white/45 hover:bg-white/[0.08] hover:text-white dark:hover:bg-white/[0.08]"
            aria-label="Copy link"
            onClick={() =>
              navigator.clipboard.writeText(href).then(
                () => {
                  setCopied(true);
                  setTimeout(() => setCopied(false), 1500);
                },
                () => toast.error("Could not copy the link"),
              )
            }
          >
            {copied ? <Check className="text-success" /> : <Copy />}
          </Button>
        </TooltipTrigger>
        <TooltipContent>{copied ? "Copied" : "Copy link"}</TooltipContent>
      </Tooltip>
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            className="size-7 shrink-0 rounded-full text-white/45 hover:bg-white/[0.08] hover:text-white dark:hover:bg-white/[0.08]"
            asChild
          >
            <a href={href} target="_blank" rel="noopener noreferrer" aria-label="Open link">
              <ExternalLink />
            </a>
          </Button>
        </TooltipTrigger>
        <TooltipContent>Open on FileCloud24</TooltipContent>
      </Tooltip>
    </div>
  );
}

function LinkStage({ job }: { job: Job }) {
  const href = safeHref(job.cloud_link);
  if (!href) return <div className="hidden lg:block" />;
  return (
    <Stage label="Link">
      <LinkChip href={href} />
    </Stage>
  );
}

function Actions({ job, onChanged }: { job: Job; onChanged: () => void }) {
  const [busy, setBusy] = useState<"retry" | "cancel" | null>(null);
  const skipped = job.upload_state === "skipped";
  const run = async (kind: "retry" | "cancel") => {
    setBusy(kind);
    try {
      if (kind === "retry") {
        const r = await retryJob(job.id);
        const title = skipped ? "Uploading to FileCloud24" : `Retrying ${r.retried}`;
        toast.success(title, { description: job.original_filename });
      } else {
        await cancelJob(job.id);
        toast("Upload cancelled", { description: "The file stays on the server and can be retried." });
      }
      onChanged();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "Request failed");
    } finally {
      setBusy(null);
    }
  };
  if (!job.can_retry && !job.can_cancel) return <div className="hidden lg:block" />;
  return (
    <div className="flex items-center justify-end gap-1.5">
      {job.can_retry ? (
        <Button
          size="sm"
          className="h-8 rounded-full bg-white px-3.5 text-black hover:bg-white/85"
          disabled={busy !== null}
          onClick={() => run("retry")}
        >
          {busy === "retry" ? <LoaderCircle className="animate-spin" /> : skipped ? <CloudUpload /> : <RotateCcw />}
          {skipped ? "Upload" : "Retry"}
        </Button>
      ) : null}
      {job.can_cancel ? (
        <AlertDialog>
          <AlertDialogTrigger asChild>
            <Button
              variant="ghost"
              size="sm"
              className="h-8 rounded-full px-3 text-white/55 hover:bg-white/[0.07] hover:text-white dark:hover:bg-white/[0.07]"
              disabled={busy !== null}
            >
              {busy === "cancel" ? <LoaderCircle className="animate-spin" /> : <X />}
              Cancel
            </Button>
          </AlertDialogTrigger>
          <AlertDialogContent className="rounded-2xl border-0 bg-[#0b0b0b] shadow-[0_30px_120px_rgba(0,0,0,1)] ring-1 ring-white/10">
            <AlertDialogHeader>
              <AlertDialogTitle>Cancel this upload?</AlertDialogTitle>
              <AlertDialogDescription className="text-white/55">
                The FileCloud24 upload session is aborted. The file stays on the server and can be retried later.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel className="rounded-full border-0 bg-white/[0.05] ring-1 ring-white/10 hover:bg-white/10 dark:bg-white/[0.05] dark:hover:bg-white/10">
                Keep uploading
              </AlertDialogCancel>
              <AlertDialogAction className="rounded-full bg-white text-black hover:bg-white/85" onClick={() => run("cancel")}>
                Cancel upload
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      ) : null}
    </div>
  );
}

function FileBlock({ job }: { job: Job }) {
  const [Icon, tile] = needsAttention(job)
    ? [TriangleAlert, "bg-destructive/10 text-destructive ring-destructive/20"]
    : job.upload_state === "uploading" || job.upload_state === "finalizing"
      ? [CloudUpload, "bg-info/10 text-info ring-info/20"]
      : isDone(job)
        ? [CircleCheck, "bg-success/10 text-success ring-success/20"]
        : [File, "bg-white/[0.04] text-white/45 ring-white/[0.08]"];
  return (
    <div className="flex min-w-0 items-start gap-3">
      <span className={cn("grid size-10 shrink-0 place-items-center rounded-xl ring-1 ring-inset", tile)}>
        <Icon className="size-[18px]" aria-hidden />
      </span>
      <div className="min-w-0 pt-0.5">
        <p className="line-clamp-2 text-sm leading-snug font-medium break-words text-white [overflow-wrap:anywhere]" title={job.original_filename}>
          {job.original_filename}
        </p>
        <p className="mt-1 text-xs text-white/40 tabular-nums">
          {formatBytes(job.size_bytes)} · {formatTime(job.download_completed_at)}
          {job.source === "manual"
            ? " · imported"
            : job.source === "history"
              ? " · recovered"
              : job.source === "ytdlp"
                ? " · yt-dlp"
                : ""}
        </p>
      </div>
    </div>
  );
}

const STAT_TONES = {
  ok: { text: "text-success", icon: "bg-success/12 text-success", glow: "rgba(63,214,139,0.11)" },
  info: { text: "text-info", icon: "bg-info/12 text-info", glow: "rgba(106,168,255,0.12)" },
  bad: { text: "text-destructive", icon: "bg-destructive/12 text-destructive", glow: "rgba(242,85,90,0.12)" },
};

function Stat({
  icon: Icon,
  label,
  value,
  tone,
  children,
}: {
  icon: React.ElementType;
  label: string;
  value: React.ReactNode;
  tone?: keyof typeof STAT_TONES;
  children?: React.ReactNode;
}) {
  const t = tone && value !== 0 ? STAT_TONES[tone] : null;
  return (
    <div
      className="relative h-full overflow-hidden rounded-2xl bg-white/[0.025] p-4 ring-1 ring-white/[0.06] sm:p-5"
      style={t ? { backgroundImage: `radial-gradient(120% 140% at 100% 0%, ${t.glow}, transparent 60%)` } : undefined}
    >
      <div className="flex items-center justify-between gap-3">
        <p className="text-[13px] font-medium text-white/50">{label}</p>
        <span className={cn("grid size-8 place-items-center rounded-lg bg-white/[0.05] text-white/45", t?.icon)}>
          <Icon className="size-4" aria-hidden />
        </span>
      </div>
      <p className={cn("mt-3 text-3xl leading-none font-semibold tracking-tight text-white tabular-nums sm:text-[34px]", t?.text)}>
        {value}
      </p>
      {children}
    </div>
  );
}

function DiskStat({ disk }: { disk: DashboardState["disk"] }) {
  const { available_bytes: free, total_bytes: total } = disk.downloads;
  const [amount, unit] = formatBytes(free).split(" ");
  const used = percent(total - free, total);
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <div className="col-span-2 lg:col-span-1">
          <Stat
            icon={HardDrive}
            label="Free space"
            tone={disk.low ? "bad" : undefined}
            value={
              <>
                {amount}
                <span className="ml-1 text-base font-medium opacity-55">{unit}</span>
              </>
            }
          >
            <span
              role="meter"
              aria-label="Download disk used"
              aria-valuenow={used}
              aria-valuemin={0}
              aria-valuemax={100}
              className="absolute inset-x-0 bottom-0 h-1 bg-white/[0.05]"
            >
              <span className={cn("block h-full", disk.low ? "bg-destructive" : "bg-white/30")} style={{ width: `${used}%` }} />
            </span>
          </Stat>
        </div>
      </TooltipTrigger>
      <TooltipContent>
        Download volume: {formatBytes(free)} of {formatBytes(total)} free · minimum {formatBytes(disk.min_free_bytes)}
      </TooltipContent>
    </Tooltip>
  );
}

function Orphans({ state, onChanged }: { state: DashboardState; onChanged: () => void }) {
  const [busy, setBusy] = useState<string | null>(null);
  if (!state.orphans.length) return null;
  return (
    <section className="rounded-2xl bg-warning/[0.03] p-4 ring-1 ring-warning/20 sm:p-5">
      <div className="flex items-start gap-3">
        <span className="grid size-9 shrink-0 place-items-center rounded-xl bg-warning/12 text-warning">
          <FolderSearch className="size-[18px]" aria-hidden />
        </span>
        <div className="min-w-0 pt-0.5">
          <p className="text-sm font-medium text-white">Unrecognized files in the download folder</p>
          <p className="mt-1 max-w-3xl text-[13px] leading-relaxed text-white/50">
            Chrome did not report these as completed downloads (for example downloads from an Incognito window, or files
            saved while the browser service was stopping). They are not uploaded unless you import them.
          </p>
        </div>
      </div>
      <ul className="mt-4 grid grid-cols-1 gap-2">
        {state.orphans.map((o) => (
          <li key={o.name} className="flex flex-wrap items-center gap-3 rounded-xl bg-black/60 px-3 py-2.5 ring-1 ring-white/[0.06]">
            <span className="grid size-8 shrink-0 place-items-center rounded-lg bg-white/[0.05] text-white/45">
              <FileDown className="size-4" aria-hidden />
            </span>
            <div className="min-w-0 flex-1 sm:flex sm:items-center sm:gap-3">
              <p className="truncate text-sm font-medium text-white sm:flex-1">{o.name}</p>
              <p className="mt-0.5 text-xs text-white/40 tabular-nums sm:mt-0">
                {formatBytes(o.size_bytes)} · {formatTime(o.modified_at)}
              </p>
            </div>
            <Button
              size="sm"
              className="h-8 w-full rounded-full bg-white px-3.5 text-black hover:bg-white/85 sm:w-auto"
              disabled={busy !== null}
              onClick={async () => {
                setBusy(o.name);
                try {
                  await importOrphan(o.name);
                  toast.success("Imported", { description: o.name });
                  onChanged();
                } catch (e) {
                  toast.error(e instanceof Error ? e.message : "Import failed");
                } finally {
                  setBusy(null);
                }
              }}
            >
              {busy === o.name ? <LoaderCircle className="animate-spin" /> : <CloudUpload />}
              Import and upload
            </Button>
          </li>
        ))}
      </ul>
    </section>
  );
}

function pageItems(page: number, pages: number): (number | "gap")[] {
  const wanted = [...new Set([1, page - 1, page, page + 1, pages])].filter((p) => p >= 1 && p <= pages).sort((a, b) => a - b);
  const out: (number | "gap")[] = [];
  let prev = 0;
  for (const p of wanted) {
    if (p - prev === 2) out.push(prev + 1);
    else if (p - prev > 2) out.push("gap");
    out.push(p);
    prev = p;
  }
  return out;
}

function Pagination({ state, onPage }: { state: DashboardState; onPage: (page: number) => Promise<void> }) {
  const [busy, setBusy] = useState(false);
  const { page, pages, per_page: perPage } = state;
  const total = state.counts.total;
  if (pages <= 1) return null;
  const go = async (p: number) => {
    if (p === page || p < 1 || p > pages) return;
    setBusy(true);
    try {
      await onPage(p);
    } finally {
      setBusy(false);
    }
  };
  const arrow =
    "size-8 rounded-full text-white/55 hover:bg-white/[0.07] hover:text-white disabled:opacity-30 dark:hover:bg-white/[0.07]";
  return (
    <nav
      aria-label="Pagination"
      className="flex flex-wrap items-center justify-between gap-3 border-t border-white/[0.05] px-4 py-3 sm:px-5"
    >
      <p className="text-xs text-white/40 tabular-nums">
        {(page - 1) * perPage + 1}–{Math.min(total, page * perPage)} of {total}
      </p>
      <div className="flex items-center gap-1">
        <Button variant="ghost" size="icon" className={arrow} aria-label="Previous page" disabled={busy || page <= 1} onClick={() => go(page - 1)}>
          <ChevronLeft />
        </Button>
        {pageItems(page, pages).map((item, i) =>
          item === "gap" ? (
            <span key={`gap-${i}`} className="w-6 text-center text-xs text-white/30">
              …
            </span>
          ) : (
            <button
              key={item}
              type="button"
              disabled={busy}
              aria-current={item === page ? "page" : undefined}
              onClick={() => go(item)}
              className={cn(
                "h-8 min-w-8 cursor-pointer rounded-full px-2 text-[13px] font-medium tabular-nums transition-colors disabled:cursor-default",
                item === page ? "bg-white text-black" : "text-white/55 hover:bg-white/[0.07] hover:text-white",
              )}
            >
              {item}
            </button>
          ),
        )}
        <Button variant="ghost" size="icon" className={arrow} aria-label="Next page" disabled={busy || page >= pages} onClick={() => go(page + 1)}>
          <ChevronRight />
        </Button>
      </div>
    </nav>
  );
}

function ClearHistory({ counts, onChanged }: { counts: DashboardState["counts"]; onChanged: () => void }) {
  const [busy, setBusy] = useState(false);
  const n = counts.clearable;
  const staying = counts.total - n;
  const clear = async () => {
    setBusy(true);
    try {
      const r = await clearHistory();
      toast.success(r.cleared === 1 ? "Cleared 1 upload from the history" : `Cleared ${r.cleared} uploads from the history`);
      onChanged();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "Could not clear the history");
    } finally {
      setBusy(false);
    }
  };
  return (
    <AlertDialog>
      <AlertDialogTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          disabled={n === 0 || busy}
          className="h-8 rounded-full bg-white/[0.04] px-3 text-[13px] text-white/75 ring-1 ring-white/[0.08] hover:bg-destructive/15 hover:text-destructive dark:hover:bg-destructive/15"
        >
          {busy ? <LoaderCircle className="animate-spin" /> : <Trash2 />}
          Clear all
        </Button>
      </AlertDialogTrigger>
      <AlertDialogContent className="rounded-2xl border-0 bg-[#0b0b0b] shadow-[0_30px_120px_rgba(0,0,0,1)] ring-1 ring-white/10">
        <AlertDialogHeader>
          <AlertDialogTitle>{n === 1 ? "Clear 1 upload from the history?" : `Clear ${n} uploads from the history?`}</AlertDialogTitle>
          <AlertDialogDescription className="text-white/55">
            Their FileCloud24 links will no longer be listed here, so copy any you still need. Nothing is deleted from
            FileCloud24.
            {staying > 0
              ? ` ${staying === 1 ? "1 entry" : `${staying} entries`} still uploading, failed or kept on the server will stay.`
              : ""}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel className="rounded-full border-0 bg-white/[0.05] ring-1 ring-white/10 hover:bg-white/10 dark:bg-white/[0.05] dark:hover:bg-white/10">
            Cancel
          </AlertDialogCancel>
          <AlertDialogAction className="rounded-full bg-white text-black hover:bg-white/85" onClick={clear}>
            Clear all
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

function sameJob(a: Job, b: Job) {
  return (Object.keys(a) as (keyof Job)[]).every((k) => a[k] === b[k]);
}

const JobRow = memo(
  function JobRow({ job, now, onChanged }: { job: Job; now: number; onChanged: () => void }) {
    return (
      <li className={cn("grid min-w-0 gap-4 px-4 py-4 transition-colors hover:bg-white/[0.015] sm:px-5 lg:items-start", COLUMNS)}>
        <FileBlock job={job} />
        <div className="grid gap-3 rounded-xl bg-white/[0.02] p-3 ring-1 ring-white/[0.05] lg:contents">
          <UploadStage job={job} now={now} />
          <CleanupStage job={job} />
          <LinkStage job={job} />
        </div>
        <Actions job={job} onChanged={onChanged} />
      </li>
    );
  },
  (a, b) => a.onChanged === b.onChanged && sameJob(a.job, b.job) && (a.job.upload_state !== "retry_wait" || a.now === b.now),
);

export function TransfersPanel({
  state,
  onChanged,
  onPage,
}: {
  state: DashboardState | null;
  onChanged: () => void;
  onPage: (page: number) => Promise<void>;
}) {
  const list = useRef<HTMLDivElement>(null);
  if (!state) {
    return (
      <div className="flex h-64 items-center justify-center text-sm text-white/40">
        <LoaderCircle className="mr-2 size-4 animate-spin" aria-hidden /> Loading…
      </div>
    );
  }
  const jobs = state.jobs;
  const { total, uploaded, active, attention } = state.counts;
  const changePage = async (p: number) => {
    await onPage(p);
    const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    list.current?.scrollIntoView({ block: "start", behavior: reduce ? "auto" : "smooth" });
  };

  return (
    <div className="grid w-full grid-cols-1 gap-4 px-3 pt-4 pb-24 sm:gap-5 sm:px-5 sm:pt-5">
      <div className="grid grid-cols-2 gap-3 lg:grid-cols-5">
        <Stat icon={Inbox} label="Downloads" value={total} />
        <Stat icon={CircleCheck} label="Uploaded" value={uploaded} tone="ok" />
        <Stat icon={Activity} label="In progress" value={active} tone="info" />
        <Stat icon={TriangleAlert} label="Need attention" value={attention} tone="bad" />
        <DiskStat disk={state.disk} />
      </div>

      <Orphans state={state} onChanged={onChanged} />

      {total === 0 ? (
        <div className="flex flex-col items-center justify-center gap-3 rounded-2xl border border-dashed border-white/10 px-6 py-20 text-center">
          <span className="grid size-14 place-items-center rounded-2xl bg-gradient-to-b from-white/[0.09] to-white/[0.02] ring-1 ring-white/10">
            <Inbox className="size-6 text-white/60" aria-hidden />
          </span>
          <p className="mt-1 font-medium text-white">No downloads yet</p>
          <p className="max-w-sm text-sm leading-relaxed text-white/45">
            Files you download in the remote browser appear here as they are uploaded.
          </p>
        </div>
      ) : (
        <div ref={list} className="scroll-mt-4 overflow-hidden rounded-2xl bg-white/[0.015] ring-1 ring-white/[0.06]">
          <div className="flex items-center justify-between gap-3 border-b border-white/[0.05] px-4 py-2.5 sm:px-5">
            <p className="text-sm font-medium text-white">History</p>
            <ClearHistory counts={state.counts} onChanged={onChanged} />
          </div>
          <div
            className={cn(
              "hidden gap-4 border-b border-white/[0.05] px-5 py-3 text-[11px] font-medium tracking-[0.08em] text-white/35 uppercase lg:grid",
              COLUMNS,
            )}
          >
            <span>File</span>
            <span>Upload</span>
            <span>Local file</span>
            <span>Link</span>
            <span />
          </div>
          <ul className="divide-y divide-white/[0.05]">
            {jobs.map((job) => (
              <JobRow key={job.id} job={job} now={state.now} onChanged={onChanged} />
            ))}
          </ul>
          <Pagination state={state} onPage={changePage} />
        </div>
      )}
    </div>
  );
}
