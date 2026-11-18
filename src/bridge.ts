export interface AttachResult {
  sessionId: number;
  width: number;
  height: number;
  autoResize?: boolean;
  name?: string;
  host?: string;
  port?: number;
  certificate?: CertificateInfo | null;
  sharedFolder?: string | null;
  audioSupported?: boolean;
  audioEnabled?: boolean;
  remapShortcuts?: boolean;
}

export interface PasteResult {
  typed: number;
  truncated: boolean;
  mode?: "clipboard" | "typed";
}

export interface CertificateInfo {
  sha256: string;
  subject?: string;
  issuer?: string;
  not_after?: string;
}

export interface RdpStateEvent {
  connectionId: string;
  sessionId?: number;
  state: "connecting" | "connected" | "reconnecting" | "disconnected" | "failed";
  error?: string;
  errorCode?: number;
  reason?: string;
  detail?: string;
}

export interface SecurityInfo {
  nla: boolean;
  host: string;
  port: number;
  certificate?: CertificateInfo | null;
}

export interface RdpClipboardEvent {
  connectionId: string;
  sessionId?: number;
  text: string;
}

export interface RdpFileEntry {
  name: string;
  relativePath?: string | null;
  size: number;
}

export interface RdpFilesEvent {
  connectionId: string;
  sessionId?: number;
  files?: RdpFileEntry[];
  clipDataId?: number | null;
}

export interface RdpFilesProgressEvent {
  connectionId: string;
  sessionId?: number;
  token: string;
  received: number;
  total: number;
}

export interface RdpFilesReadyEvent {
  connectionId: string;
  sessionId?: number;
  token: string;
  name: string;
  size: number;
}

export interface RdpFilesErrorEvent {
  connectionId: string;
  sessionId?: number;
  token: string;
  message: string;
}

export interface RdpStatsEvent {
  connectionId: string;
  sessionId?: number;
  fps: number;
  fullFps: number;
  rects: number;
  kbps: number;
  width: number;
  height: number;
}

export function api(): Window["dbxPlugin"] {
  return window.dbxPlugin;
}

export function connectionContext(): { connectionId: string; surface: string } {
  const context = api().context ?? {};
  const connectionId = typeof context.connectionId === "string" ? context.connectionId : "";
  const surface = typeof context.surface === "string" ? context.surface : "tab";
  return { connectionId, surface };
}

export async function attach(connectionId: string, width: number, height: number): Promise<AttachResult> {
  return await api().invoke<AttachResult>("rdp/attach", {
    connectionId,
    viewport: { width: Math.round(width), height: Math.round(height) },
  });
}

export async function detach(connectionId: string, sessionId: number): Promise<void> {
  try {
    await api().invoke("rdp/detach", { connectionId, sessionId });
  } catch {
    // The tab may be closing while the sidecar already dropped the session.
  }
}

export async function requestFullFrame(connectionId: string): Promise<void> {
  try {
    await api().invoke("rdp/requestFullFrame", { connectionId });
  } catch {
    // A missing session is surfaced through rdp/state.
  }
}

export async function resize(connectionId: string, width: number, height: number): Promise<void> {
  try {
    await api().invoke("rdp/resize", {
      connectionId,
      width: Math.round(width),
      height: Math.round(height),
    });
  } catch {
    // Older hosts/servers may not support dynamic resize.
  }
}

export async function paste(connectionId: string, text: string): Promise<PasteResult> {
  return await api().invoke<PasteResult>("rdp/paste", { connectionId, text });
}

/** Advertises local text on the remote clipboard without pasting it. */
export async function advertiseClipboard(connectionId: string, text: string): Promise<void> {
  await api().invoke("rdp/clipboard/local", { connectionId, text });
}

export type ClipboardReadResult =
  | { ok: true; text: string }
  | { ok: false; reason: "unsupported" | "denied" | "empty" };

export async function readClipboard(): Promise<ClipboardReadResult> {
  const capabilities = api().capabilities ?? {};
  if (capabilities.clipboardRead === false) return { ok: false, reason: "unsupported" };
  const clipboard = api().clipboard;
  if (!clipboard || typeof clipboard.readText !== "function") {
    return { ok: false, reason: "unsupported" };
  }
  try {
    const text = await clipboard.readText();
    return text ? { ok: true, text } : { ok: false, reason: "empty" };
  } catch {
    return { ok: false, reason: "denied" };
  }
}

export function closeWorkbench(): void {
  const close = api().closeWorkbench;
  if (typeof close !== "function") return;
  void close().catch(() => {
    // Closing is best-effort; the host may already have torn the tab down.
  });
}

export async function reopenConnection(connectionId: string): Promise<void> {
  const reopen = api().reopenConnection;
  if (typeof reopen !== "function") {
    throw new Error("Reconnect is not supported by this DBX host");
  }
  await reopen(connectionId);
}

export function sendInput(data: Uint8Array): void {
  const sendBinary = api().sendBinary;
  if (typeof sendBinary !== "function") return;
  void sendBinary("rdp-input-v1", data).catch(() => {
    // Input is best-effort while the session is reconnecting.
  });
}

export function copyToLocal(text: string): void {
  const copy = api().copy;
  if (typeof copy !== "function") return;
  void copy(text).catch(() => {
    // Clipboard writes are not permission-gated; a failure is non-fatal.
  });
}
