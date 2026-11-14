import {
  Archive,
  Ban,
  CircleCheck,
  CircleDashed,
  CircleX,
  Clock,
  CloudOff,
  FileQuestion,
  LoaderCircle,
  RotateCw,
  Trash2,
  type LucideIcon,
} from "lucide-react";

import { cn } from "@/lib/utils";
import type { CleanupState, UploadState } from "@/lib/api";

export type Tone = "ok" | "warn" | "bad" | "info" | "idle";

const TONES: Record<Tone, string> = {
  ok: "bg-success/[0.09] text-success ring-success/20",
  warn: "bg-warning/[0.09] text-warning ring-warning/20",
  bad: "bg-destructive/[0.09] text-destructive ring-destructive/25",
  info: "bg-info/[0.09] text-info ring-info/20",
  idle: "bg-white/[0.04] text-white/55 ring-white/[0.08]",
};

type Spec = { label: string; tone: Tone; icon: LucideIcon; spin?: boolean };

const UPLOAD: Record<UploadState, Spec> = {
  waiting: { label: "Claiming", tone: "idle", icon: CircleDashed },
  pending: { label: "Queued", tone: "info", icon: Clock },
  uploading: { label: "Uploading", tone: "info", icon: LoaderCircle, spin: true },
  finalizing: { label: "Finalizing", tone: "info", icon: LoaderCircle, spin: true },
  retry_wait: { label: "Retrying", tone: "warn", icon: RotateCw },
  uploaded: { label: "Uploaded", tone: "ok", icon: CircleCheck },
  failed: { label: "Failed", tone: "bad", icon: CircleX },
  cancelled: { label: "Cancelled", tone: "idle", icon: Ban },
  skipped: { label: "Not uploaded", tone: "idle", icon: CloudOff },
};

const CLEANUP: Record<CleanupState, Spec> = {
  waiting: { label: "Kept until done", tone: "idle", icon: CircleDashed },
  pending: { label: "Deleting", tone: "info", icon: LoaderCircle, spin: true },
  retry_wait: { label: "Retrying delete", tone: "warn", icon: RotateCw },
  deleted: { label: "Deleted", tone: "ok", icon: Trash2 },
  retained: { label: "Kept on server", tone: "idle", icon: Archive },
  failed: { label: "Delete failed", tone: "bad", icon: CircleX },
};

const MISSING: Spec = { label: "Missing", tone: "bad", icon: FileQuestion };

export function Pill({ tone, icon: Icon, spin, children, className }: { tone: Tone; icon?: LucideIcon; spin?: boolean; children: React.ReactNode; className?: string }) {
  return (
    <span
      className={cn(
        "inline-flex h-6 items-center gap-1.5 whitespace-nowrap rounded-full px-2.5 text-xs font-medium ring-1 ring-inset",
        TONES[tone],
        className,
      )}
    >
      {Icon ? <Icon className={cn("size-3.5 shrink-0", spin && "animate-spin")} aria-hidden /> : null}
      {children}
    </span>
  );
}

function SpecBadge({ spec }: { spec: Spec }) {
  return (
    <Pill tone={spec.tone} icon={spec.icon} spin={spec.spin}>
      {spec.label}
    </Pill>
  );
}

export const UploadBadge = ({ state }: { state: UploadState }) => <SpecBadge spec={UPLOAD[state] ?? UPLOAD.waiting} />;
export const CleanupBadge = ({ state, missing }: { state: CleanupState; missing?: boolean }) => (
  <SpecBadge spec={missing ? MISSING : (CLEANUP[state] ?? CLEANUP.waiting)} />
);
