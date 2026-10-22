<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";

import RemoteCanvas from "./RemoteCanvas.vue";
import SecurityPanel from "./SecurityPanel.vue";
import {
  api,
  attach,
  connectionContext,
  copyToLocal,
  detach,
  paste,
  readClipboardText,
  reopenConnection,
  requestFullFrame,
  resize,
  sendClipboardText,
  type AttachResult,
  type RdpClipboardEvent,
  type RdpStateEvent,
  type SecurityInfo,
} from "./bridge";
import { createTranslator, detectLocale, type Locale, type MessageKey } from "./i18n";

type UiState = "idle" | "empty" | "connecting" | "connected" | "reconnecting" | "disconnected" | "failed";

const canvasRef = ref<InstanceType<typeof RemoteCanvas> | null>(null);
const canvasHost = ref<HTMLDivElement | null>(null);
const connectionId = ref("");
const sessionId = ref<number | null>(null);
const desktopWidth = ref(1280);
const desktopHeight = ref(800);
const connectionName = ref("RDP");
const host = ref("");
const port = ref(3389);
const state = ref<UiState>("idle");
const error = ref("");
const security = ref<SecurityInfo | null>(null);
const showSecurity = ref(false);
const clipboardSync = ref(true);
const locale = ref<Locale>("en");
const toastMessage = ref("");
const attaching = ref(false);
const initialized = ref(false);

const t = computed(() => createTranslator(locale.value));
const unsubscribers: Array<() => void> = [];
let toastTimer: number | null = null;
let attachRetryTimer: number | null = null;

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

onMounted(async () => {
  applyTheme(api().theme);
  unsubscribers.push(
    api().onBinary(({ channel, data }) => {
      if (channel === "rdp-frame-v1") {
        canvasRef.value?.handleBinary(data);
      }
    }),
  );
  unsubscribers.push(api().onEvent(handleEvent));
  const onInit = api().onInit;
  if (typeof onInit === "function") {
    unsubscribers.push(
      onInit((init) => {
        if (init?.locale) locale.value = detectLocale(init.locale);
        if (init?.theme) applyTheme(init.theme);
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
  const context = connectionContext();
  connectionId.value = context.connectionId;
  locale.value = detectLocale(api().locale);
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
  const availableWidth = Math.max(1, element?.clientWidth ?? 1280);
  const availableHeight = Math.max(1, element?.clientHeight ?? 800);
  const width = Math.max(1024, Math.min(2560, Math.round(availableWidth)));
  const height = Math.max(600, Math.min(1600, Math.round((width * availableHeight) / availableWidth)));
  return { width, height };
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
    return true;
  } catch (failure) {
    const message = describeError(failure);
    if (showFailure) {
      state.value = "failed";
      error.value = message;
    } else if (state.value !== "reconnecting") {
      state.value = "disconnected";
      error.value = "";
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
  if (previousConnection && previousSession !== null) {
    void detach(previousConnection, previousSession);
  }
  state.value = "connecting";
  error.value = "";
  await ensureAttached(false);
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
    case "rdp/reconnecting":
      state.value = "reconnecting";
      break;
    case "rdp/resizeFallback":
      showToast(t.value("app.resizeFallback"));
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
      }
      break;
    case "failed":
      if (sessionId.value === null || incomingSession === undefined || incomingSession === sessionId.value) {
        state.value = "failed";
        error.value = event.error ?? t.value("app.retryHint");
        sessionId.value = null;
      }
      break;
    case "disconnected":
      if (sessionId.value === null || incomingSession === undefined || incomingSession === sessionId.value) {
        state.value = "disconnected";
        sessionId.value = null;
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
  if (!event.text) return;
  copyToLocal(event.text);
  showToast(t.value("app.clipboardCopied"));
}

async function reconnect() {
  if (!connectionId.value) return;
  state.value = "connecting";
  error.value = "";
  try {
    await reopenConnection(connectionId.value);
  } catch (failure) {
    state.value = "failed";
    error.value = describeError(failure);
    return;
  }
  for (let attempt = 0; attempt < 10; attempt += 1) {
    if (await ensureAttached(true)) return;
    await new Promise<void>((resolve) => {
      attachRetryTimer = window.setTimeout(resolve, 500);
    });
  }
  state.value = "failed";
  if (!error.value) error.value = t.value("app.retryHint");
}

async function onPasteRequest() {
  if (!connectionId.value) return;
  const text = await readClipboardText();
  if (text === null) {
    showToast(t.value("app.clipboardDenied"));
    canvasRef.value?.sendCtrlV();
    return;
  }
  if (!text) {
    showToast(t.value("app.clipboardEmpty"));
    canvasRef.value?.sendCtrlV();
    return;
  }
  try {
    await paste(connectionId.value, text);
    showToast(t.value("app.clipboardSent"));
  } catch {
    canvasRef.value?.sendCtrlV();
  }
}

async function sendClipboard() {
  if (!connectionId.value) return;
  const text = await readClipboardText();
  if (text === null) {
    showToast(t.value("app.clipboardDenied"));
    return;
  }
  if (!text) {
    showToast(t.value("app.clipboardEmpty"));
    return;
  }
  try {
    await sendClipboardText(connectionId.value, text);
    showToast(t.value("app.clipboardSent"));
  } catch (failure) {
    showToast(describeError(failure));
  }
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
</script>

<template>
  <div class="rdp-app">
    <header class="toolbar">
      <div class="toolbar__identity">
        <span :class="statusClass" />
        <span class="toolbar__title">{{ connectionName }}</span>
        <span v-if="host" class="toolbar__subtitle">{{ host }}:{{ port }}</span>
      </div>
      <div class="toolbar__actions">
        <button type="button" @click="canvasRef?.sendCtrlAltDel()">{{ t("app.ctrlAltDel") }}</button>
        <button type="button" @click="canvasRef?.sendEscape()">Esc</button>
        <button type="button" @click="canvasRef?.sendMeta()">Win</button>
        <button type="button" @click="sendClipboard">{{ t("app.sendClipboard") }}</button>
        <button type="button" :class="{ 'button--active': clipboardSync }" @click="toggleClipboardSync">
          {{ clipboardSync ? t("app.clipboardOn") : t("app.clipboardOff") }}
        </button>
        <button type="button" :class="{ 'button--active': showSecurity }" @click="showSecurity = !showSecurity">
          {{ t("app.security") }}
        </button>
        <button type="button" class="button--primary" @click="reconnect">{{ t("app.reconnect") }}</button>
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
          @desktop-size="onDesktopSize"
          @resize-request="onResizeRequest"
          @full-frame-request="onFullFrameRequest"
          @paste-request="onPasteRequest"
        />
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
          </div>
        </div>
        <div v-else-if="state === 'failed' || state === 'disconnected'" class="overlay">
          <div class="overlay__card">
            <h2>{{ t(stateLabel) }}</h2>
            <p v-if="error" class="overlay__error">{{ error }}</p>
            <p>{{ t("app.retryHint") }}</p>
            <button type="button" class="button--primary" @click="reconnect">{{ t("app.reconnect") }}</button>
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
      <span>{{ desktopWidth }} × {{ desktopHeight }}</span>
      <span>{{ clipboardSync ? t("app.clipboardOn") : t("app.clipboardOff") }}</span>
    </footer>

    <div v-if="toastMessage" class="toast">{{ toastMessage }}</div>
  </div>
</template>
