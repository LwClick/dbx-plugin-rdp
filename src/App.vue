<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";

import RemoteCanvas from "./RemoteCanvas.vue";
import SecurityPanel from "./SecurityPanel.vue";
import {
  advertiseClipboard,
  api,
  attach,
  closeWorkbench,
  connectionContext,
  copyToLocal,
  detach,
  paste,
  readClipboard,
  reopenConnection,
  requestFullFrame,
  resize,
  type AttachResult,
  type RdpClipboardEvent,
  type RdpFilesErrorEvent,
  type RdpFilesEvent,
  type RdpFilesProgressEvent,
  type RdpFilesReadyEvent,
  type RdpStateEvent,
  type RdpStatsEvent,
  type SecurityInfo,
} from "./bridge";
import {
  createClipboardAnnouncer,
  MAX_CLIPBOARD_IMAGE_BYTES,
  pasteImageToRemote,
  readClipboardImage,
  saveRemoteImage,
  type ClipboardImageEvent,
  type PasteContent,
} from "./clipboard";
import { createTranslator, detectLocale, type Locale, type MessageKey } from "./i18n";
import { cursorCss } from "./cursor";
import { CURSOR_CHANNEL, parseCursorMessage, type CursorMessage } from "./cursorProtocol";
import { targetDesktopSize } from "./viewport";
import {
  fileTransferAvailable,
  formatBytes,
  saveRemoteFile,
  uploadFiles,
  type RemoteFileEntry,
  type UploadFile,
} from "./files";

type UiState = "idle" | "empty" | "connecting" | "connected" | "reconnecting" | "disconnected" | "failed";

const canvasRef = ref<InstanceType<typeof RemoteCanvas> | null>(null);
const canvasHost = ref<HTMLDivElement | null>(null);
const connectionId = ref("");
const sessionId = ref<number | null>(null);
const desktopWidth = ref(1920);
const desktopHeight = ref(1080);
const connectionName = ref("RDP");
const host = ref("");
const port = ref(3389);
const state = ref<UiState>("idle");
const error = ref("");
const security = ref<SecurityInfo | null>(null);
const showSecurity = ref(false);
const clipboardSync = ref(true);
const clipboardReadable = ref(true);
const locale = ref<Locale>("en");
const toastMessage = ref("");
const attaching = ref(false);
const initialized = ref(false);
const resizeUnsupported = ref(false);
const autoResize = ref(true);
const cursorStyle = ref("default");
const fileTransferReady = ref(false);
const fileDropActive = ref(false);
const fileBusy = ref(false);
const uploadAbort = ref<AbortController | null>(null);
const fileOffer = ref<RemoteFileEntry[] | null>(null);
const imageOffer = ref<ClipboardImageEvent | null>(null);
const pendingDownload = ref<{ token: string; total: number; bucket: number } | null>(null);
const disconnectReason = ref("");
const disconnectDetail = ref("");
const isMacPlatform = /Mac/i.test(navigator.userAgent);
const pasteShortcutLabel = isMacPlatform ? "Cmd+V" : "Ctrl+V";
const fullscreenShortcutLabel = isMacPlatform ? "Ctrl+Cmd+F" : "F11";
const ZOOM_STEPS = [50, 75, 100, 125, 150, 200] as const;
type ZoomLevel = "fit" | (typeof ZOOM_STEPS)[number];
const zoom = ref<ZoomLevel>("fit");
const showStats = ref(false);
const stats = ref<RdpStatsEvent | null>(null);
const sharedFolder = ref("");
const canvasFocused = ref(true);
const remapShortcuts = ref(true);
const reconnectAttempt = ref(0);
const RECONNECT_MAX = 10;
const appRoot = ref<HTMLDivElement | null>(null);
const fullscreen = ref(false);
const immersive = ref(false);
const floatingToolbarVisible = ref(false);

const t = computed(() => createTranslator(locale.value));
const unsubscribers: Array<() => void> = [];
let toastTimer: number | null = null;
let attachRetryTimer: number | null = null;
let attachRetryResolve: (() => void) | null = null;
let reconnectCancelled = false;
let floatingTimer: number | null = null;
let fileTransferBound = false;

const announceClipboard = createClipboardAnnouncer(
  async () => {
    if (!connectionId.value || state.value !== "connected" || !clipboardSync.value) return null;
    const result = await readClipboard();
    return result.ok ? result.text : null;
  },
  async (text) => {
    if (connectionId.value) await advertiseClipboard(connectionId.value, text);
  },
);

function setupFileTransfer() {
  fileTransferReady.value = fileTransferAvailable();
  if (fileTransferBound) return;
  const transfer = api().fileTransfer;
  if (!transfer) return;
  fileTransferBound = true;
  unsubscribers.push(
    transfer.onDragState((active) => {
      fileDropActive.value = active;
    }),
  );
  unsubscribers.push(
    transfer.onDrop((files) => {
      fileDropActive.value = false;
      void sendFiles(
        files.map((file) => ({
          handleId: file.handleId,
          name: file.name,
          size: file.size,
          relativePath: file.relativePath,
        })),
      );
    }),
  );
}

function zoomStorageKey(): string {
  return `rdpZoom:${connectionId.value}`;
}

async function loadZoom() {
  const storage = api().storage;
  if (!storage || typeof storage.get !== "function") return;
  try {
    const value = await storage.get<string>(zoomStorageKey());
    if (value === "fit") {
      zoom.value = "fit";
      return;
    }
    const level = Number(value);
    if ((ZOOM_STEPS as readonly number[]).includes(level)) {
      zoom.value = level as ZoomLevel;
    }
  } catch {
    // Older hosts may not implement host.storage.
  }
}

async function setZoom(value: string) {
  const level = value === "fit" ? "fit" : Number(value);
  if (level !== "fit" && !(ZOOM_STEPS as readonly number[]).includes(level)) return;
  zoom.value = level as ZoomLevel;
  const storage = api().storage;
  if (!storage || typeof storage.set !== "function") return;
  try {
    await storage.set(zoomStorageKey(), value);
  } catch {
    // Zoom still applies for this session.
  }
}

const zoomLabel = computed(() => (zoom.value === "fit" ? t.value("app.zoomFit") : `${zoom.value}%`));

function zoomIn() {
  if (zoom.value === "fit") {
    void setZoom("125");
    return;
  }
  const next = ZOOM_STEPS.find((step) => step > (zoom.value as number));
  void setZoom(String(next ?? ZOOM_STEPS[ZOOM_STEPS.length - 1]));
}

function zoomOut() {
  if (zoom.value === "fit") {
    void setZoom("75");
    return;
  }
  const index = ZOOM_STEPS.indexOf(zoom.value as (typeof ZOOM_STEPS)[number]);
  const next = index > 0 ? ZOOM_STEPS[index - 1] : ZOOM_STEPS[0];
  void setZoom(String(next));
}

function onZoomShortcut(action: "in" | "out" | "reset") {
  if (action === "in") zoomIn();
  else if (action === "out") zoomOut();
  else void setZoom("fit");
}

const stateLabel = computed<MessageKey>(() => {
  switch (state.value) {
    case "connecting":
      return "app.connecting";
    case "connected":
      return "app.connected";
    case "reconnecting":
      return "app.reconnecting";
    case "failed":
      return "app.failed";
    case "disconnected":
      return "app.disconnected";
    default:
      return "app.empty";
  }
});

const statusClass = computed(() => {
  switch (state.value) {
    case "connected":
      return "status-dot status-dot--ok";
    case "connecting":
    case "reconnecting":
      return "status-dot status-dot--busy";
    case "failed":
      return "status-dot status-dot--error";
    default:
      return "status-dot";
  }
});

const reconnecting = computed(() => state.value === "connecting" || state.value === "reconnecting");

const reconnectProgress = computed(() =>
  t.value("app.reconnectProgress")
    .replace("{attempt}", String(Math.max(1, reconnectAttempt.value)))
    .replace("{max}", String(RECONNECT_MAX)),
);

const securityLabel = computed(() => {
  if (!security.value) return "";
  return security.value.nla ? "NLA" : "TLS";
});

const sessionChrome = computed(() => fullscreen.value || immersive.value);

const canCloseTab = computed(() => typeof api().closeWorkbench === "function");

const fileOfferSize = computed(() =>
  (fileOffer.value ?? []).reduce((sum, file) => sum + (Number.isFinite(file.size) ? file.size : 0), 0),
);

const DISCONNECT_REASONS = [
  "another-connection",
  "admin-disconnect",
  "idle-timeout",
  "session-timeout",
  "server-shutdown",
  "server-reboot",
  "server-memory",
  "server-denied",
  "server-initiated",
  "logoff",
  "terminated",
  "other",
] as const;

type DisconnectReason = (typeof DISCONNECT_REASONS)[number];

function disconnectKey(suffix: "title" | "detail"): MessageKey | null {
  const reason = disconnectReason.value as DisconnectReason;
  if (!DISCONNECT_REASONS.includes(reason)) return null;
  return `disconnect.${reason}.${suffix}` as MessageKey;
}

const disconnectTitle = computed(() => {
  const key = disconnectKey("title");
  if (key) return t.value(key);
  return state.value === "failed" ? t.value("app.failed") : t.value("app.disconnected");
});

const disconnectMessage = computed(() => {
  const key = disconnectKey("detail");
  if (key) return t.value(key);
  return disconnectDetail.value || t.value("app.disconnectedHint");
});

onMounted(async () => {
  applyTheme(api().theme);
  unsubscribers.push(
    api().onBinary(({ channel, data }) => {
      if (channel === "rdp-frame-v1") {
        canvasRef.value?.handleBinary(data);
      } else if (channel === CURSOR_CHANNEL) {
        handleCursor(data);
      }
    }),
  );
  unsubscribers.push(api().onEvent(handleEvent));
  setupFileTransfer();
  const onInit = api().onInit;
  if (typeof onInit === "function") {
    unsubscribers.push(
      onInit((init) => {
        if (init?.locale) locale.value = detectLocale(init.locale);
        if (init?.theme) applyTheme(init.theme);
        if (init?.capabilities && typeof init.capabilities.clipboardRead === "boolean") {
          clipboardReadable.value = init.capabilities.clipboardRead;
        }
        if (initialized.value && connectionId.value && state.value !== "connected") {
          void ensureAttached(false);
        }
      }),
    );
  }
  const onContext = api().onContext;
  if (typeof onContext === "function") {
    unsubscribers.push(
      onContext((context) => {
        const next = typeof context.connectionId === "string" ? context.connectionId : "";
        if (next && next !== connectionId.value) {
          void switchConnection(next);
        }
      }),
    );
  }

  await api().ready;
  setupFileTransfer();
  window.addEventListener("focus", onWindowFocus);
  document.addEventListener("visibilitychange", onDocumentVisible);
  document.addEventListener("fullscreenchange", onFullscreenChange);
  window.addEventListener("keydown", onGlobalKeyDown, true);
  const context = connectionContext();
  connectionId.value = context.connectionId;
  locale.value = detectLocale(api().locale);
  clipboardReadable.value = api().capabilities?.clipboardRead !== false;
  applyTheme(api().theme);
  const storage = api().storage;
  if (storage && typeof storage.get === "function") {
    try {
      const stored = await storage.get<boolean>("clipboardSync");
      if (typeof stored === "boolean") clipboardSync.value = stored;
    } catch {
      // Older hosts may not implement host.storage.
    }
  }
  await loadZoom();

  if (!connectionId.value) {
    state.value = "empty";
    initialized.value = true;
    return;
  }
  state.value = "connecting";
  await ensureAttached(false);
  initialized.value = true;
});

onBeforeUnmount(() => {
  window.removeEventListener("focus", onWindowFocus);
  document.removeEventListener("visibilitychange", onDocumentVisible);
  document.removeEventListener("fullscreenchange", onFullscreenChange);
  window.removeEventListener("keydown", onGlobalKeyDown, true);
  if (floatingTimer !== null) window.clearTimeout(floatingTimer);
  if (document.fullscreenElement) void document.exitFullscreen().catch(() => undefined);
  for (const unsubscribe of unsubscribers.splice(0)) {
    try {
      unsubscribe();
    } catch {
      // The bridge may already be torn down.
    }
  }
  if (toastTimer !== null) window.clearTimeout(toastTimer);
  if (attachRetryTimer !== null) window.clearTimeout(attachRetryTimer);
  if (connectionId.value && sessionId.value !== null) {
    void detach(connectionId.value, sessionId.value);
  }
});

function applyTheme(theme: { tokens?: Record<string, string> } | undefined) {
  const tokens = theme?.tokens;
  if (!tokens) return;
  for (const [name, value] of Object.entries(tokens)) {
    if (name.startsWith("--color") || name.startsWith("--radius") || name.startsWith("--font")) {
      document.documentElement.style.setProperty(name, value);
    }
  }
}

function targetViewport(): { width: number; height: number } {
  const element = canvasHost.value;
  const availableWidth = element?.clientWidth ?? 0;
  const availableHeight = element?.clientHeight ?? 0;
  // A restored workbench tab can be hidden (0x0) while the plugin boots.
  // Keep the current desktop size instead of collapsing the remote session to
  // the 1024x600 minimum before the tab becomes visible.
  if (availableWidth <= 0 || availableHeight <= 0) {
    return { width: desktopWidth.value, height: desktopHeight.value };
  }
  return targetDesktopSize(availableWidth, availableHeight);
}

async function ensureAttached(showFailure: boolean): Promise<boolean> {
  if (!connectionId.value || attaching.value) return false;
  attaching.value = true;
  try {
    const viewport = targetViewport();
    const result: AttachResult = await attach(connectionId.value, viewport.width, viewport.height);
    const previousSession = sessionId.value;
    sessionId.value = result.sessionId;
    desktopWidth.value = result.width;
    desktopHeight.value = result.height;
    autoResize.value = result.autoResize !== false;
    sharedFolder.value = typeof result.sharedFolder === "string" ? result.sharedFolder : "";
    remapShortcuts.value = result.remapShortcuts !== false;
    connectionName.value = result.name ?? connectionName.value;
    host.value = result.host ?? host.value;
    port.value = result.port ?? port.value;
    state.value = "connected";
    error.value = "";
    if (previousSession !== null && previousSession !== result.sessionId) {
      void detach(connectionId.value, previousSession);
    }
    const setTitle = api().setTitle;
    if (typeof setTitle === "function") {
      void setTitle(result.name ?? "RDP");
    }
    try {
      security.value = await api().invoke<SecurityInfo>("rdp/security", { connectionId: connectionId.value });
    } catch {
      security.value = null;
    }
    refocusCanvasSoon();
    announceClipboard(true);
    return true;
  } catch (failure) {
    const message = describeError(failure);
    error.value = message;
    if (showFailure) {
      state.value = "failed";
    } else if (state.value !== "reconnecting") {
      state.value = "disconnected";
    }
    return false;
  } finally {
    attaching.value = false;
  }
}

async function switchConnection(next: string) {
  const previousConnection = connectionId.value;
  const previousSession = sessionId.value;
  connectionId.value = next;
  sessionId.value = null;
  resizeUnsupported.value = false;
  autoResize.value = true;
  cursorStyle.value = "default";
  fileOffer.value = null;
  imageOffer.value = null;
  pendingDownload.value = null;
  fileBusy.value = false;
  disconnectReason.value = "";
  disconnectDetail.value = "";
  stats.value = null;
  sharedFolder.value = "";
  await loadZoom();
  if (previousConnection && previousSession !== null) {
    void detach(previousConnection, previousSession);
  }
  state.value = "connecting";
  error.value = "";
  await ensureAttached(false);
}

function handleCursor(data: Uint8Array) {
  let message: CursorMessage;
  try {
    message = parseCursorMessage(data);
  } catch {
    return;
  }
  // Attach replays the cached cursor before the invoke response resolves, so
  // accept the message while sessionId is still null.
  if (sessionId.value !== null && message.sessionId !== sessionId.value) return;
  cursorStyle.value = cursorCss(message);
}

function handleEvent(event: { method: string; params: unknown }) {
  const params = (event.params ?? {}) as Record<string, unknown>;
  if (params.connectionId !== connectionId.value) return;
  switch (event.method) {
    case "rdp/state":
      handleState(params as unknown as RdpStateEvent);
      break;
    case "rdp/clipboard":
      handleRemoteClipboard(params as unknown as RdpClipboardEvent);
      break;
    case "rdp/clipboard/image":
      handleRemoteImage(params as unknown as ClipboardImageEvent);
      break;
    case "rdp/files":
      handleRemoteFiles(params as unknown as RdpFilesEvent);
      break;
    case "rdp/files/progress":
      handleFileProgress(params as unknown as RdpFilesProgressEvent);
      break;
    case "rdp/files/ready":
      void handleFileReady(params as unknown as RdpFilesReadyEvent);
      break;
    case "rdp/files/error":
      handleFileError(params as unknown as RdpFilesErrorEvent);
      break;
    case "rdp/reconnecting":
      state.value = "reconnecting";
      break;
    case "rdp/resizeFallback":
      if (!resizeUnsupported.value) showToast(t.value("app.resizeFallback"));
      resizeUnsupported.value = true;
      break;
    case "rdp/stats":
      stats.value = params as unknown as RdpStatsEvent;
      break;
    default:
      break;
  }
}

function handleState(event: RdpStateEvent) {
  const incomingSession = typeof event.sessionId === "number" ? event.sessionId : undefined;
  switch (event.state) {
    case "connected":
      if (incomingSession !== undefined && incomingSession !== sessionId.value) {
        void ensureAttached(true);
      } else if (sessionId.value === null) {
        void ensureAttached(true);
      } else {
        state.value = "connected";
        error.value = "";
        disconnectReason.value = "";
        disconnectDetail.value = "";
      }
      break;
    case "failed":
      if (sessionId.value === null || incomingSession === undefined || incomingSession === sessionId.value) {
        state.value = "failed";
        error.value = event.error ?? t.value("app.retryHint");
        sessionId.value = null;
        stats.value = null;
      }
      break;
    case "disconnected":
      if (sessionId.value === null || incomingSession === undefined || incomingSession === sessionId.value) {
        state.value = "disconnected";
        disconnectReason.value = event.reason ?? "";
        disconnectDetail.value = event.detail ?? "";
        fileOffer.value = null;
        imageOffer.value = null;
        pendingDownload.value = null;
        fileBusy.value = false;
        sessionId.value = null;
        stats.value = null;
      }
      break;
    case "reconnecting":
      state.value = "reconnecting";
      break;
    case "connecting":
      state.value = "connecting";
      break;
    default:
      break;
  }
}

function handleRemoteClipboard(event: RdpClipboardEvent) {
  if (!clipboardSync.value) return;
  if (sessionId.value !== null && event.sessionId !== undefined && event.sessionId !== sessionId.value) return;
  fileOffer.value = null;
  imageOffer.value = null;
  if (!event.text) return;
  copyToLocal(event.text);
  showToast(t.value("app.clipboardCopied"));
}

function handleRemoteImage(event: ClipboardImageEvent) {
  if (sessionId.value !== null && event.sessionId !== undefined && event.sessionId !== sessionId.value) return;
  if (!event.token) return;
  fileOffer.value = null;
  imageOffer.value = event;
}

function handleRemoteFiles(event: RdpFilesEvent) {
  if (sessionId.value !== null && event.sessionId !== undefined && event.sessionId !== sessionId.value) return;
  const files = (event.files ?? []).filter((file) => typeof file.name === "string" && file.name.length > 0);
  if (files.length === 0) {
    fileOffer.value = null;
    return;
  }
  if (!fileTransferReady.value) {
    fileOffer.value = null;
    showToast(t.value("app.fileUnsupported"));
    return;
  }
  fileOffer.value = files;
}

function handleFileProgress(event: RdpFilesProgressEvent) {
  const pending = pendingDownload.value;
  if (!pending || pending.token !== event.token) return;
  const total = event.total > 0 ? event.total : pending.total;
  if (total <= 0) return;
  const percent = Math.min(100, Math.floor((event.received / total) * 100));
  const bucket = Math.floor(percent / 10) * 10;
  if (bucket <= pending.bucket) return;
  pending.bucket = bucket;
  showToast(t.value("app.fileDownloadProgress").replace("{percent}", String(bucket)));
}

async function handleFileReady(event: RdpFilesReadyEvent) {
  const pending = pendingDownload.value;
  if (!pending || pending.token !== event.token || !connectionId.value) return;
  pendingDownload.value = null;
  fileBusy.value = false;
  fileOffer.value = null;
  try {
    const saved = await saveRemoteFile(connectionId.value, event.token, event.name, event.size);
    if (saved) {
      showToast(t.value("app.fileSaved").replace("{name}", event.name));
    }
  } catch (failure) {
    showToast(t.value("app.fileSaveFailed").replace("{error}", describeError(failure)));
  }
}

function handleFileError(event: RdpFilesErrorEvent) {
  const pending = pendingDownload.value;
  // A very fast failure can race the RPC response that carries the token.
  if (pending && pending.token !== event.token) return;
  pendingDownload.value = null;
  fileBusy.value = false;
  showToast(event.message || t.value("app.fileUnsupported"));
}

async function pickFiles() {
  const transfer = api().fileTransfer;
  if (!transfer || fileBusy.value) return;
  try {
    const { files } = await transfer.pick({ multiple: true });
    await sendFiles(files);
  } catch (failure) {
    showToast(describeError(failure));
  }
}

async function sendFiles(files: UploadFile[]) {
  if (!connectionId.value || files.length === 0 || fileBusy.value || state.value !== "connected") {
    const transfer = api().fileTransfer;
    for (const file of files) {
      // Dropped files arrive with an open handle; release it when the workbench
      // is not ready to upload, otherwise the host keeps the file open.
      void transfer?.cancel(file.handleId).catch(() => undefined);
    }
    return;
  }
  fileBusy.value = true;
  fileOffer.value = null;
  imageOffer.value = null;
  const controller = new AbortController();
  uploadAbort.value = controller;
  let bucket = -1;
  const uploads: UploadFile[] = files.map((file) => ({
    handleId: file.handleId,
    name: file.name,
    size: file.size,
    relativePath: file.relativePath,
  }));
  try {
    const count = await uploadFiles(
      connectionId.value,
      uploads,
      ({ sent, total }) => {
        const percent = total > 0 ? Math.min(100, Math.floor((sent / total) * 100)) : 0;
        const next = Math.floor(percent / 10) * 10;
        if (next <= bucket) return;
        bucket = next;
        showToast(t.value("app.fileUploadProgress").replace("{percent}", String(next)));
      },
      controller.signal,
    );
    showToast(t.value("app.fileUploaded").replace("{count}", String(count)));
    canvasRef.value?.focusInput();
  } catch (failure) {
    if (controller.signal.aborted) {
      showToast(t.value("app.uploadCancelled"));
    } else {
      showToast(t.value("app.fileUploadFailed").replace("{error}", describeError(failure)));
    }
  } finally {
    if (uploadAbort.value === controller) uploadAbort.value = null;
    fileBusy.value = false;
  }
}

function cancelUpload() {
  uploadAbort.value?.abort();
}

async function downloadRemoteFiles() {
  if (!connectionId.value || !fileOffer.value || fileBusy.value) return;
  const total = fileOfferSize.value;
  try {
    const { token } = await api().invoke<{ token: string }>("rdp/files/download", {
      connectionId: connectionId.value,
    });
    pendingDownload.value = { token, total, bucket: -1 };
    fileBusy.value = true;
    showToast(t.value("app.fileDownloading"));
  } catch (failure) {
    showToast(describeError(failure));
  }
}

function dismissFileOffer() {
  fileOffer.value = null;
}

function closeCurrentTab() {
  closeWorkbench();
}

async function reconnect() {
  if (!connectionId.value || reconnecting.value || attaching.value) return;
  state.value = "connecting";
  error.value = "";
  disconnectReason.value = "";
  disconnectDetail.value = "";
  fileOffer.value = null;
  imageOffer.value = null;
  resizeUnsupported.value = false;
  autoResize.value = true;
  cursorStyle.value = "default";
  reconnectCancelled = false;
  reconnectAttempt.value = 0;
  try {
    await reopenConnection(connectionId.value);
  } catch (failure) {
    state.value = "failed";
    error.value = describeError(failure);
    return;
  }
  for (let attempt = 1; attempt <= RECONNECT_MAX; attempt += 1) {
    if (reconnectCancelled) return;
    reconnectAttempt.value = attempt;
    state.value = "reconnecting";
    const attached = await ensureAttached(false);
    if (reconnectCancelled) return;
    if (attached) {
      reconnectAttempt.value = 0;
      return;
    }
    state.value = "reconnecting";
    if (attempt < RECONNECT_MAX) await waitAttachRetry();
  }
  reconnectAttempt.value = 0;
  if (reconnectCancelled) return;
  state.value = "failed";
  if (!error.value) error.value = t.value("app.retryHint");
}

function waitAttachRetry(): Promise<void> {
  return new Promise<void>((resolve) => {
    attachRetryResolve = resolve;
    attachRetryTimer = window.setTimeout(() => {
      attachRetryTimer = null;
      attachRetryResolve = null;
      resolve();
    }, 500);
  });
}

async function cancelReconnect() {
  reconnectCancelled = true;
  if (attachRetryTimer !== null) {
    window.clearTimeout(attachRetryTimer);
    attachRetryTimer = null;
  }
  const resolve = attachRetryResolve;
  attachRetryResolve = null;
  resolve?.();
  reconnectAttempt.value = 0;
  state.value = "disconnected";
  disconnectReason.value = "";
  disconnectDetail.value = "";
  if (!connectionId.value) return;
  try {
    // Drops the sidecar session so its auto-reconnect loop stops as well.
    await api().invoke("rdp/disconnect", { connectionId: connectionId.value });
  } catch {
    // The session may already be gone.
  }
}

async function pasteText(text: string) {
  if (!connectionId.value || !text) return;
  try {
    const result = await paste(connectionId.value, text);
    if (result.mode === "clipboard") {
      showToast(
        result.truncated
          ? t.value("app.pasteClipboardTruncated")
          : t.value("app.pasteClipboardDone"),
      );
      return;
    }
    const template = result.truncated ? t.value("app.pasteTruncated") : t.value("app.pasteSent");
    showToast(template.replace("{count}", String(result.typed)));
  } catch (failure) {
    showToast(describeError(failure));
  }
}

function onPasteText(text: string) {
  void pasteText(text);
}

function onPasteContent(content: PasteContent) {
  if (!content.image) {
    if (content.text) void pasteText(content.text);
    return;
  }
  void pasteRemoteImage(new Uint8Array(content.image.bytes), content.text ?? "");
}

async function pasteRemoteImage(bytes: Uint8Array, text: string) {
  if (!connectionId.value || sessionId.value === null) return;
  if (bytes.length > MAX_CLIPBOARD_IMAGE_BYTES) {
    showToast(t.value("app.imageTooLarge"));
    return;
  }
  try {
    await pasteImageToRemote(connectionId.value, sessionId.value, bytes, text);
    showToast(t.value("app.imagePasted"));
  } catch (failure) {
    showToast(describeError(failure));
  }
}

async function tryPasteClipboardImage(): Promise<boolean> {
  const image = await readClipboardImage();
  if (!image) return false;
  await pasteRemoteImage(image, "");
  return true;
}

function onPasteError(reason: "unsupported" | "denied" | "empty" | "invalid-image") {
  if (reason === "unsupported") {
    showToast(t.value("app.clipboardUnsupported"));
  } else if (reason === "denied") {
    showToast(t.value("app.clipboardDenied"));
  } else if (reason === "invalid-image") {
    showToast(t.value("app.imageFailed"));
  } else {
    void tryPasteClipboardImage().then((pasted) => {
      if (pasted) return;
      showToast(t.value("app.clipboardEmpty"));
      canvasRef.value?.sendCtrlV();
    });
  }
}

async function pasteClipboard() {
  if (!connectionId.value) return;
  const result = await readClipboard();
  if (result.ok) {
    await pasteText(result.text);
    return;
  }
  if (result.reason === "empty") {
    if (await tryPasteClipboardImage()) return;
    showToast(t.value("app.clipboardEmpty"));
    canvasRef.value?.sendCtrlV();
    return;
  }
  if (result.reason === "unsupported") {
    showToast(t.value("app.clipboardUnsupported"));
  } else {
    showToast(t.value("app.clipboardDenied"));
  }
}

async function saveImage() {
  if (!connectionId.value || !imageOffer.value || fileBusy.value) return;
  const offer = imageOffer.value;
  fileBusy.value = true;
  try {
    const saved = await saveRemoteImage(connectionId.value, offer.token, offer.name, offer.size);
    if (saved) showToast(t.value("app.imageSaved").replace("{name}", offer.name));
  } catch (failure) {
    showToast(t.value("app.imageSaveFailed").replace("{error}", describeError(failure)));
  } finally {
    fileBusy.value = false;
    imageOffer.value = null;
  }
}

function dismissImageOffer() {
  imageOffer.value = null;
}

async function toggleClipboardSync() {
  clipboardSync.value = !clipboardSync.value;
  const storage = api().storage;
  if (storage && typeof storage.set === "function") {
    try {
      await storage.set("clipboardSync", clipboardSync.value);
    } catch {
      // Persistence is best-effort.
    }
  }
  showToast(clipboardSync.value ? t.value("app.clipboardOn") : t.value("app.clipboardOff"));
}

async function onResizeRequest(width: number, height: number) {
  if (!connectionId.value) return;
  await resize(connectionId.value, width, height);
}

async function onFullFrameRequest() {
  if (!connectionId.value) return;
  await requestFullFrame(connectionId.value);
}

function onDesktopSize(width: number, height: number) {
  desktopWidth.value = width;
  desktopHeight.value = height;
}

function showToast(message: string) {
  toastMessage.value = message;
  if (toastTimer !== null) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    toastMessage.value = "";
    toastTimer = null;
  }, 3000);
}

function describeError(failure: unknown): string {
  if (failure instanceof Error) return failure.message;
  if (typeof failure === "string") return failure;
  try {
    return JSON.stringify(failure);
  } catch {
    return String(failure);
  }
}

function onCanvasFocus(focused: boolean) {
  canvasFocused.value = focused;
}

function onToolbarPointerUp(event: PointerEvent) {
  // The zoom <select> needs the pointer sequence to open its native menu.
  if ((event.target as HTMLElement | null)?.closest("select")) return;
  refocusCanvasSoon();
}

function refocusCanvasSoon() {
  // Toolbar clicks move DOM focus to the button; return it after the click
  // handler runs so typing continues inside the remote session.
  window.setTimeout(() => canvasRef.value?.focusInput(), 0);
}

function onWindowFocus() {
  if (state.value === "connected") {
    refocusCanvasSoon();
    announceClipboard();
  }
}

function onDocumentVisible() {
  if (document.visibilityState === "visible" && state.value === "connected") {
    refocusCanvasSoon();
    announceClipboard();
  }
}

async function toggleFullscreen() {
  if (document.fullscreenElement) {
    try {
      await document.exitFullscreen();
    } catch {
      // The host may have already left fullscreen.
    }
    return;
  }
  if (immersive.value) {
    immersive.value = false;
    hideFloatingToolbar();
    refocusCanvasSoon();
    return;
  }
  const root = appRoot.value;
  if (!root || document.fullscreenEnabled === false || typeof root.requestFullscreen !== "function") {
    immersive.value = true;
    showFloatingToolbar();
    refocusCanvasSoon();
    return;
  }
  try {
    await root.requestFullscreen();
    showFloatingToolbar();
  } catch {
    // The host iframe may not grant fullscreen; fill the workbench instead.
    immersive.value = true;
    showFloatingToolbar();
  }
  refocusCanvasSoon();
}

function onFullscreenChange() {
  fullscreen.value = document.fullscreenElement !== null && document.fullscreenElement === appRoot.value;
  if (fullscreen.value) {
    immersive.value = false;
    showFloatingToolbar();
  } else if (!immersive.value) {
    hideFloatingToolbar();
  }
  refocusCanvasSoon();
}

function showFloatingToolbar() {
  floatingToolbarVisible.value = true;
  if (floatingTimer !== null) window.clearTimeout(floatingTimer);
  floatingTimer = window.setTimeout(() => {
    floatingToolbarVisible.value = false;
    floatingTimer = null;
  }, 2000);
}

function hideFloatingToolbarSoon() {
  if (floatingTimer !== null) window.clearTimeout(floatingTimer);
  floatingTimer = window.setTimeout(() => {
    floatingToolbarVisible.value = false;
    floatingTimer = null;
  }, 800);
}

function hideFloatingToolbar() {
  if (floatingTimer !== null) {
    window.clearTimeout(floatingTimer);
    floatingTimer = null;
  }
  floatingToolbarVisible.value = false;
}

function onGlobalKeyDown(event: KeyboardEvent) {
  // Ctrl+Cmd+F on macOS and F11 elsewhere toggle the workbench fullscreen
  // before the canvas can forward the combo to the remote session.
  const combo = isMacPlatform
    ? event.ctrlKey && event.metaKey && !event.altKey && event.code === "KeyF"
    : event.key === "F11";
  if (!combo) return;
  event.preventDefault();
  event.stopPropagation();
  void toggleFullscreen();
}

function onAppPointerMove(event: PointerEvent) {
  // Reveal the floating toolbar near the top edge without stealing clicks from
  // the remote desktop (no transparent hot-zone overlay).
  if (sessionChrome.value && event.clientY <= 40) showFloatingToolbar();
}
</script>

<template>
  <div
    ref="appRoot"
    class="rdp-app"
    :class="{ 'rdp-app--chrome': sessionChrome }"
    @pointermove="onAppPointerMove"
  >
    <header class="toolbar" @pointerup="onToolbarPointerUp">
      <div class="toolbar__identity">
        <span :class="statusClass" />
        <span class="toolbar__title">{{ connectionName }}</span>
        <span v-if="host" class="toolbar__subtitle">{{ host }}:{{ port }}</span>
        <span
          v-if="sharedFolder && state === 'connected'"
          class="toolbar__badge"
          :title="t('app.sharedFolderHint').replace('{path}', sharedFolder)"
        >{{ t("app.sharedFolder") }}</span>
      </div>
      <div class="toolbar__actions">
        <button type="button" @click="canvasRef?.sendCtrlAltDel()">{{ t("app.ctrlAltDel") }}</button>
        <button type="button" @click="canvasRef?.sendEscape()">Esc</button>
        <button type="button" @click="canvasRef?.sendMeta()">Win</button>
        <button type="button" :title="pasteShortcutLabel" :disabled="!clipboardReadable" @click="pasteClipboard">{{ t("app.paste") }}</button>
        <button
          v-if="fileTransferReady"
          type="button"
          :disabled="fileBusy || state !== 'connected'"
          @click="pickFiles"
        >
          {{ t("app.sendFiles") }}
        </button>
        <button v-if="uploadAbort" type="button" @click="cancelUpload">
          {{ t("app.cancelUpload") }}
        </button>
        <button type="button" :class="{ 'button--active': clipboardSync }" @click="toggleClipboardSync">
          {{ clipboardSync ? t("app.clipboardOn") : t("app.clipboardOff") }}
        </button>
        <div class="toolbar__zoom">
          <button type="button" :title="t('app.zoomOut')" @click="zoomOut">−</button>
          <select
            class="toolbar__select"
            :value="zoom === 'fit' ? 'fit' : String(zoom)"
            :title="t('app.zoom')"
            @change="setZoom(($event.target as HTMLSelectElement).value)"
          >
            <option value="fit">{{ t("app.zoomFit") }}</option>
            <option v-for="step in ZOOM_STEPS" :key="step" :value="String(step)">{{ step }}%</option>
          </select>
          <button type="button" :title="t('app.zoomIn')" @click="zoomIn">+</button>
        </div>
        <button type="button" :class="{ 'button--active': showStats }" @click="showStats = !showStats">
          {{ t("app.stats") }}
        </button>
        <button type="button" :class="{ 'button--active': showSecurity }" @click="showSecurity = !showSecurity">
          {{ t("app.security") }}
        </button>
        <button type="button" :title="fullscreenShortcutLabel" @click="toggleFullscreen">
          {{ t("app.fullscreen") }}
        </button>
        <button type="button" class="button--primary" :disabled="reconnecting" @click="reconnect">{{ t("app.reconnect") }}</button>
      </div>
    </header>

    <main class="content">
      <div ref="canvasHost" class="canvas-host">
        <RemoteCanvas
          v-if="connectionId"
          ref="canvasRef"
          :session-id="sessionId"
          :desktop-width="desktopWidth"
          :desktop-height="desktopHeight"
          :auto-resize="autoResize && !resizeUnsupported"
          :cursor-style="cursorStyle"
          :zoom="zoom"
          :remap-shortcuts="remapShortcuts"
          @desktop-size="onDesktopSize"
          @resize-request="onResizeRequest"
          @full-frame-request="onFullFrameRequest"
          @paste-text="onPasteText"
          @paste-content="onPasteContent"
          @paste-error="onPasteError"
          @focused="onCanvasFocus"
          @zoom-change="onZoomShortcut"
        />
        <button
          v-if="state === 'connected' && !canvasFocused"
          type="button"
          class="focus-hint"
          @click="canvasRef?.focusInput()"
        >{{ t("app.clickToType") }}</button>
        <div v-if="showStats && stats" class="stats-panel">
          <div><span>{{ t("app.statsFps") }}</span><b>{{ stats.fps }}</b></div>
          <div><span>{{ t("app.statsFull") }}</span><b>{{ stats.fullFps }}</b></div>
          <div><span>{{ t("app.statsRects") }}</span><b>{{ stats.rects }}</b></div>
          <div><span>{{ t("app.statsKbps") }}</span><b>{{ stats.kbps }} kbps</b></div>
          <div><span>{{ t("app.statsSize") }}</span><b>{{ stats.width }} × {{ stats.height }}</b></div>
        </div>
        <div v-if="fileOffer" class="file-banner">
          <span class="file-banner__text">
            {{
              t("app.remoteFiles")
                .replace("{count}", String(fileOffer.length))
                .replace("{size}", formatBytes(fileOfferSize))
            }}
          </span>
          <button type="button" class="button--primary" :disabled="fileBusy" @click="downloadRemoteFiles">
            {{ t("app.saveToLocal") }}
          </button>
          <button type="button" class="file-banner__close" :disabled="fileBusy" @click="dismissFileOffer">×</button>
        </div>
        <div v-if="imageOffer" class="file-banner">
          <span class="file-banner__text">
            {{
              t("app.remoteImage")
                .replace("{width}", String(imageOffer.width))
                .replace("{height}", String(imageOffer.height))
                .replace("{size}", formatBytes(imageOffer.size))
            }}
          </span>
          <button type="button" class="button--primary" :disabled="fileBusy" @click="saveImage">
            {{ t("app.saveImage") }}
          </button>
          <button type="button" class="file-banner__close" :disabled="fileBusy" @click="dismissImageOffer">×</button>
        </div>
        <div v-if="fileDropActive" class="drop-overlay">
          <span>{{ t("app.dropFiles") }}</span>
        </div>
        <div v-if="!connectionId || state === 'empty'" class="overlay">
          <div class="overlay__card">
            <h2>{{ t("app.empty") }}</h2>
            <p>{{ t("error.noConnectionId") }}</p>
          </div>
        </div>
        <div
          v-else-if="state === 'connecting' || state === 'reconnecting'"
          class="overlay overlay--soft"
        >
          <div class="overlay__card overlay__card--compact">
            <span class="spinner" />
            <span>{{ t(stateLabel) }}</span>
            <span v-if="state === 'reconnecting'" class="overlay__progress">
              {{ reconnectProgress }}
            </span>
            <button v-if="state === 'reconnecting'" type="button" @click="cancelReconnect">
              {{ t("app.cancelReconnect") }}
            </button>
          </div>
        </div>
        <div v-else-if="state === 'failed' || state === 'disconnected'" class="overlay">
          <div class="overlay__card">
            <h2>{{ state === "disconnected" ? disconnectTitle : t("app.failed") }}</h2>
            <p v-if="state === 'failed' && error" class="overlay__error">{{ error }}</p>
            <p v-else-if="state === 'disconnected'" class="overlay__error">{{ disconnectMessage }}</p>
            <p v-if="state === 'disconnected'">{{ t("app.disconnectedHint") }}</p>
            <p v-else>{{ t("app.retryHint") }}</p>
            <div class="overlay__actions">
              <button type="button" class="button--primary" :disabled="reconnecting" @click="reconnect">
                <span v-if="reconnecting" class="spinner" />
                {{ t("app.reconnect") }}
              </button>
              <button v-if="canCloseTab" type="button" @click="closeCurrentTab">{{ t("app.closeTab") }}</button>
            </div>
          </div>
        </div>
        <SecurityPanel
          v-if="showSecurity"
          :security="security"
          :t="t"
          @close="showSecurity = false"
        />
      </div>
    </main>

    <footer class="statusbar">
      <span>{{ t(stateLabel) }}</span>
      <span v-if="host">{{ host }}:{{ port }}</span>
      <span v-if="securityLabel">{{ securityLabel }}</span>
      <span>{{ desktopWidth }} × {{ desktopHeight }}</span>
      <span v-if="resizeUnsupported">{{ t("app.scaledDisplay") }}</span>
      <span>{{ clipboardSync ? t("app.clipboardOn") : t("app.clipboardOff") }}</span>
    </footer>

    <div
      v-if="sessionChrome"
      class="fs-toolbar"
      :class="{ 'fs-toolbar--visible': floatingToolbarVisible }"
      @pointerenter="showFloatingToolbar"
      @pointerleave="hideFloatingToolbarSoon"
      @pointerup="onToolbarPointerUp"
    >
      <button type="button" @click="canvasRef?.sendCtrlAltDel()">{{ t("app.ctrlAltDel") }}</button>
      <button type="button" :disabled="!clipboardReadable" @click="pasteClipboard">{{ t("app.paste") }}</button>
      <button type="button" :title="t('app.zoomOut')" @click="zoomOut">−</button>
      <button type="button" class="fs-toolbar__zoom" @click="setZoom('fit')">{{ zoomLabel }}</button>
      <button type="button" :title="t('app.zoomIn')" @click="zoomIn">+</button>
      <button type="button" class="button--primary" @click="toggleFullscreen">
        {{ t("app.exitFullscreen") }}
      </button>
    </div>

    <div v-if="toastMessage" class="toast">{{ toastMessage }}</div>
  </div>
</template>
