
export type UploadState =
  | "waiting"
  | "pending"
  | "uploading"
  | "finalizing"
  | "retry_wait"
  | "uploaded"
  | "failed"
  | "cancelled"
  | "skipped";
export type CleanupState = "waiting" | "pending" | "retry_wait" | "deleted" | "retained" | "failed";
export type FileState = "claiming" | "staged" | "missing" | "deleted";

export interface Job {
  id: string;
  download_guid: string;
  source: "extension" | "cdp" | "history" | "manual" | "ytdlp";
  original_filename: string;
  chrome_filename: string;
  size_bytes: number;
  download_completed_at: number;
  created_at: number;
  updated_at: number;
  file_state: FileState;
  upload_state: UploadState;
  upload_attempts: number;
  upload_next_at: number | null;
  upload_error: string | null;
  remote_chunks_done: number;
  uploaded_bytes: number;
  cloud_link: string | null;
  uploaded_at: number | null;
  cleanup_state: CleanupState;
  cleanup_attempts: number;
  cleanup_next_at: number | null;
  cleanup_error: string | null;
  cleaned_at: number | null;
  progress_bytes: number;
  can_retry: boolean;
  can_cancel: boolean;
}

export interface DiskSpace {
  available_bytes: number;
  total_bytes: number;
}

export interface ActiveDownload {
  guid: string;
  filename: string;
  received_bytes: number;
  total_bytes: number;
  started_at_ms: number;
}

export interface RejectedDownload {
  filename: string;
  reason: string;
  at_ms: number;
}

export interface AgentStatus {
  updated_at_ms: number;
  agent_started_at_ms: number;
  chrome_running: boolean;
  chrome_version: string | null;
  chrome_starts: number;
  sandbox_ok: boolean | null;
  sandbox_detail: string | null;
  downloads_paused_low_disk: boolean;
  disk: DiskSpace;
  min_free_disk_bytes: number;
  active_downloads: ActiveDownload[];
  recent_rejections: RejectedDownload[];
  last_error: string | null;
  monitor_connected?: boolean;
  monitor_detail?: string | null;
  opened_link?: { url: string; at_ms: number } | null;
}

export interface Orphan {
  name: string;
  size_bytes: number;
  modified_at: number;
}

export interface JobCounts {
  total: number;
  uploaded: number;
  active: number;
  attention: number;
  clearable: number;
}

export interface DashboardState {
  now: number;
  jobs: Job[];
  page: number;
  pages: number;
  per_page: number;
  counts: JobCounts;
  disk: { downloads: DiskSpace; state: DiskSpace; min_free_bytes: number; low: boolean };
  browser: { status: AgentStatus | null; stale: boolean };
  orphans: Orphan[];
  display: DisplayView;
  https?: { expires_at_ms: number | null; error: string | null } | null;
  settings: {
    delete_after_success: boolean;
    filecloud_base_url: string;
    max_upload_size: number | null;
    max_retry_attempts: number;
  };
}

export type StreamMode = "vnc" | "webrtc";
export type DeviceMode = "default" | StreamMode;

export interface DisplayView {
  stream_mode: StreamMode;
  display_scale: number;
  vnc_quality: number;
  vnc_compression: number;
}

export interface Settings extends DisplayView {
  filecloud: boolean;
  keep_local: boolean;
  webrtc_codec: "auto" | "av1" | "h265" | "h264" | "vp9" | "vp8";
  webrtc_fps: number;
  webrtc_quality: "low" | "medium" | "high" | "max";
  webrtc_host: string;
}

export interface SettingsResponse {
  values: Settings;
  defaults: Settings;
  webrtc_ready: boolean;
}

export interface SessionInfo {
  id: string;
  ip: string | null;
  user_agent: string | null;
  created_at: number;
  last_seen_at: number;
  current: boolean;
}

export interface CodeView {
  url: string | null;
  port: number;
}

export interface Session {
  user: string;
  csrf: string;
  display: DisplayView;
  code: CodeView | null;
}

export class ApiError extends Error {
  constructor(
    message: string,
    public status: number,
  ) {
    super(message);
  }
}

let csrfToken = "";

function toLogin() {
  const next = window.location.pathname + window.location.search;
  window.location.assign(`/login?next=${encodeURIComponent(next)}`);
}

async function request<T>(method: "GET" | "POST", path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { Accept: "application/json" };
  if (method !== "GET") headers["X-CSRF-Token"] = csrfToken;
  if (body !== undefined) headers["Content-Type"] = "application/json";
  const res = await fetch(path, {
    method,
    headers,
    credentials: "same-origin",
    cache: "no-store",
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (res.status === 401) {
    toLogin();
    throw new ApiError("Signed out", 401);
  }
  const data = (await res.json().catch(() => ({}))) as { error?: string } & T;
  if (!res.ok) throw new ApiError(data.error ?? `HTTP ${res.status}`, res.status);
  return data;
}

export async function getSession(): Promise<Session> {
  const s = await request<Session>("GET", "/api/session");
  csrfToken = s.csrf;
  return s;
}

export const getState = (page = 1) => request<DashboardState>("GET", `/api/state?page=${page}`);
export const retryJob = (id: string) =>
  request<{ retried: string }>("POST", `/api/jobs/${encodeURIComponent(id)}/retry`);
export const cancelJob = (id: string) =>
  request<{ cancelled: boolean }>("POST", `/api/jobs/${encodeURIComponent(id)}/cancel`);
export const clearHistory = () => request<{ cleared: number }>("POST", "/api/jobs/clear");
export const importOrphan = (name: string) => request<{ job_id: string }>("POST", "/api/orphans/import", { name });
export const logout = () => request<{ ok: boolean }>("POST", "/logout");
export const getSettings = () => request<SettingsResponse>("GET", "/api/settings");
export const saveSettings = (patch: Partial<Settings>) => request<SettingsResponse>("POST", "/api/settings", patch);
export const getSessions = () => request<{ sessions: SessionInfo[] }>("GET", "/api/sessions");
export const logoutSession = (id: string) =>
  request<{ logged_out: boolean }>("POST", `/api/sessions/${encodeURIComponent(id)}/logout`);
export const logoutOtherSessions = () => request<{ logged_out: number }>("POST", "/api/sessions/logout-others");

export async function login(username: string, password: string): Promise<void> {
  const res = await fetch("/login", {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify({ username, password }),
  });
  const data = (await res.json().catch(() => null)) as { status?: string; message?: string } | null;
  if (!res.ok || data?.status !== "success") {
    throw new ApiError(data?.message || "Login failed.", res.status);
  }
}

export function safeHref(url: string | null): string | null {
  if (!url) return null;
  try {
    const u = new URL(url);
    return u.protocol === "https:" || u.protocol === "http:" ? u.href : null;
  } catch {
    return null;
  }
}
