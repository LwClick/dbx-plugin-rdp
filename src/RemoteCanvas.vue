<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

import {
  FRAME_TYPE_BITMAP,
  FRAME_TYPE_RESIZE,
  chunkRowCount,
  frameMatchesSurface,
  isFullDesktopFrame,
  splitFrame,
} from "./frameProtocol";
import { scaleRgbaNearest } from "./pixels";
import { targetDesktopSize } from "./viewport";
import {
  focusSyncPacket,
  keyPacket,
  mouseButtonPacket,
  mouseMovePacket,
  rdpButton,
  releaseAllPacket,
  unicodePacket,
  wheelPacket,
  wheelUnits,
} from "./input";
import { readClipboard, sendInput } from "./bridge";
import type { PasteContent } from "./clipboard";
import { ALT, CTRL, DELETE_KEY, META, V_KEY, scancodeForCode } from "./scancodes";
import { remapMacCommand, shortcutTapKeys } from "./shortcuts";

const props = withDefaults(
  defineProps<{
    sessionId: number | null;
    desktopWidth: number;
    desktopHeight: number;
    autoResize?: boolean;
    cursorStyle?: string;
    zoom?: "fit" | number;
    remapShortcuts?: boolean;
  }>(),
  { autoResize: true, cursorStyle: "default", zoom: "fit", remapShortcuts: true },
);

const emit = defineEmits<{
  (event: "desktop-size", width: number, height: number): void;
  (event: "resize-request", width: number, height: number): void;
  (event: "full-frame-request"): void;
  (event: "paste-text", text: string): void;
  (event: "paste-content", content: PasteContent): void;
  (event: "paste-error", reason: "unsupported" | "denied" | "empty" | "invalid-image"): void;
  (event: "focused", value: boolean): void;
  (event: "zoom-change", action: "in" | "out" | "reset"): void;
}>();

const stage = ref<HTMLDivElement | null>(null);
const canvas = ref<HTMLCanvasElement | null>(null);
const inputSink = ref<HTMLTextAreaElement | null>(null);

let context: CanvasRenderingContext2D | null = null;
let imageData: ImageData | null = null;
let surfaceWidth = 0;
let surfaceHeight = 0;
let dirty: { x: number; y: number; width: number; height: number } | null = null;
let drawScheduled = false;
let pending: { seq: number; total: number; received: number; full: boolean } | null = null;
let awaitingFull = true;
let fullRetryTimer: number | null = null;
let resizeTimer: number | null = null;
let lastRequestedSize: { width: number; height: number } | null = null;
let mouseFrame: number | null = null;
let pendingMouse: { x: number; y: number } | null = null;
let wheelFrame: number | null = null;
const pendingWheel = { vertical: true, delta: 0, x: 0, y: 0 };
let interceptedPaste = false;
const suppressedZoomKeys = new Set<string>();
let resizeObserver: ResizeObserver | null = null;
const pressedButtons = new Set<number>();
let lastRemote = { x: 0, y: 0 };
let pasteToken = 0;
let pasteFallbackTimer: number | null = null;
let lastPaste = { text: "", at: 0 };
// The sandboxed workbench may not deliver a native paste event on every host.
// Give it a short head start, then fall back to the gated host clipboard API.
const PASTE_FALLBACK_MS = 150;
const PASTE_DEDUPE_MS = 500;
// macOS Cmd maps to the remote Windows key, so forwarding it would tap Start
// and steal focus. Cmd+V must stay untouched so the native paste event fires.
const isMacPlatform = typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);

const focused = ref(false);
const stageWidth = ref(0);
const stageHeight = ref(0);
const surfaceVersion = ref(0);
const modifierState = { scrollLock: false, numLock: false, capsLock: false, kanaLock: false };

const canvasStyle = computed(() => {
  // `surfaceVersion` makes this computed re-run when the remote framebuffer is
  // recreated even though the pixel dimensions are plain variables.
  void surfaceVersion.value;
  const cursor = props.cursorStyle || "default";
  if (surfaceWidth === 0 || surfaceHeight === 0) return { cursor };
  const availableWidth = stageWidth.value || surfaceWidth;
  const availableHeight = stageHeight.value || surfaceHeight;
  // Always fit the stage, including upscaling. A smaller remote framebuffer
  // must never leave permanent black bars around the workbench.
  const scale =
    typeof props.zoom === "number"
      ? Math.max(0.1, props.zoom / 100)
      : Math.min(availableWidth / surfaceWidth, availableHeight / surfaceHeight);
  return {
    width: `${Math.max(1, Math.floor(surfaceWidth * scale))}px`,
    height: `${Math.max(1, Math.floor(surfaceHeight * scale))}px`,
    cursor,
  };
});

const manualZoom = computed(() => typeof props.zoom === "number");

onMounted(() => {
  resetSurface(props.desktopWidth, props.desktopHeight);
  resizeObserver = new ResizeObserver(() => {
    fitToStage();
    scheduleRemoteResize();
  });
  if (stage.value) resizeObserver.observe(stage.value);
  window.addEventListener("resize", onWindowResize);
  window.addEventListener("mouseup", onGlobalMouseUp);
  window.addEventListener("blur", releasePressedButtons);
  document.addEventListener("visibilitychange", onVisibilityChange);
});

onBeforeUnmount(() => {
  resizeObserver?.disconnect();
  resizeObserver = null;
  releasePressedButtons();
  window.removeEventListener("resize", onWindowResize);
  window.removeEventListener("mouseup", onGlobalMouseUp);
  window.removeEventListener("blur", releasePressedButtons);
  document.removeEventListener("visibilitychange", onVisibilityChange);
  if (resizeTimer !== null) window.clearTimeout(resizeTimer);
  if (pasteFallbackTimer !== null) window.clearTimeout(pasteFallbackTimer);
  stopFullRetry();
  if (mouseFrame !== null) cancelAnimationFrame(mouseFrame);
  if (wheelFrame !== null) cancelAnimationFrame(wheelFrame);
});

watch(
  () => [props.desktopWidth, props.desktopHeight] as const,
  ([width, height]) => {
    if (width === surfaceWidth && height === surfaceHeight) return;
    adoptSize(width, height);
    requestFullWithRetry();
  },
);

watch(
  () => props.sessionId,
  (sessionId) => {
    stopFullRetry();
    if (sessionId !== null) {
      awaitingFull = true;
      requestFullWithRetry();
      scheduleRemoteResize();
    }
  },
);

watch(
  () => props.autoResize,
  (enabled) => {
    if (enabled) scheduleRemoteResize();
  },
);

function onVisibilityChange() {
  fitToStage();
  releasePressedButtons();
  scheduleRemoteResize();
}

function onWindowResize() {
  fitToStage();
  scheduleRemoteResize();
}

function resetSurface(width: number, height: number, preserve = false) {
  const element = canvas.value;
  if (!element || width <= 0 || height <= 0) return;
  const previous =
    preserve && imageData && surfaceWidth > 0 && surfaceHeight > 0
      ? { data: imageData.data, width: surfaceWidth, height: surfaceHeight }
      : null;
  surfaceWidth = width;
  surfaceHeight = height;
  element.width = width;
  element.height = height;
  context = element.getContext("2d", { alpha: false });
  if (!context) return;
  context.imageSmoothingEnabled = false;
  imageData = context.createImageData(width, height);
  if (previous) {
    // Keep the previous picture (scaled) until the server repaints. Clearing
    // to black here is what turned a dropped resize frame into a black screen.
    imageData.data.set(
      scaleRgbaNearest(previous.data, previous.width, previous.height, width, height),
    );
  } else {
    for (let index = 3; index < imageData.data.length; index += 4) {
      imageData.data[index] = 255;
    }
  }
  context.putImageData(imageData, 0, 0);
  dirty = null;
  pending = null;
  awaitingFull = true;
  surfaceVersion.value += 1;
  fitToStage();
}

function adoptSize(width: number, height: number) {
  if (width === surfaceWidth && height === surfaceHeight) return;
  lastRequestedSize = null;
  resetSurface(width, height, true);
  emit("desktop-size", width, height);
}

function fitToStage() {
  const element = stage.value;
  if (!element) return;
  stageWidth.value = element.clientWidth;
  stageHeight.value = element.clientHeight;
}

function scheduleRemoteResize() {
  if (resizeTimer !== null) window.clearTimeout(resizeTimer);
  resizeTimer = window.setTimeout(() => {
    resizeTimer = null;
    const element = stage.value;
    if (!element || props.sessionId === null || !props.autoResize) return;
    const availableWidth = element.clientWidth;
    const availableHeight = element.clientHeight;
    if (availableWidth < 320 || availableHeight < 240) return;
    const { width, height } = targetDesktopSize(availableWidth, availableHeight);
    if (
      lastRequestedSize &&
      Math.abs(width - lastRequestedSize.width) <= 24 &&
      Math.abs(height - lastRequestedSize.height) <= 24
    ) {
      return;
    }
    if (Math.abs(width - props.desktopWidth) > 24 || Math.abs(height - props.desktopHeight) > 24) {
      lastRequestedSize = { width, height };
      emit("resize-request", width, height);
    }
  }, 300);
}

function handleBinary(data: Uint8Array) {
  let fragment;
  try {
    fragment = splitFrame(data);
  } catch {
    return;
  }
  const header = fragment.header;
  if (props.sessionId === null || header.sessionId !== props.sessionId) return;

  if (header.frameType === FRAME_TYPE_RESIZE) {
    if (header.desktopWidth === surfaceWidth && header.desktopHeight === surfaceHeight) return;
    adoptSize(header.desktopWidth, header.desktopHeight);
    requestFullWithRetry();
    return;
  }
  if (header.frameType !== FRAME_TYPE_BITMAP || header.chunkCount === 0) return;

  const full = isFullDesktopFrame(header);
  if (!imageData || !frameMatchesSurface(header, surfaceWidth, surfaceHeight)) {
    // The host drops binary messages that race the iframe load. If the resize
    // notification or the first full frame is lost, adopt the size carried by
    // the next bitmap instead of discarding every later update.
    adoptSize(header.desktopWidth, header.desktopHeight);
    if (!full) {
      requestFullWithRetry();
      return;
    }
  }
  if (pending && header.seq !== pending.seq) {
    if (pending.received < pending.total) {
      pending = null;
      awaitingFull = true;
      requestFullWithRetry();
      return;
    }
    pending = null;
  }
  if (awaitingFull && !full) {
    requestFullWithRetry();
    return;
  }
  if (!pending) {
    pending = { seq: header.seq, total: header.chunkCount, received: 0, full };
  }
  if (header.chunkIndex !== pending.received) {
    pending = null;
    awaitingFull = true;
    requestFullWithRetry();
    return;
  }

  if (!imageData) {
    awaitingFull = true;
    requestFullWithRetry();
    return;
  }
  const rows = chunkRowCount(header, fragment.payload.length);
  if (rows <= 0 || header.width === 0) return;
  const rowBytes = header.width * 4;
  const destination = imageData.data;
  const destinationRow = header.y + header.rowStart;
  for (let row = 0; row < rows; row += 1) {
    const sourceStart = row * rowBytes;
    const destinationStart = ((destinationRow + row) * surfaceWidth + header.x) * 4;
    destination.set(fragment.payload.subarray(sourceStart, sourceStart + rowBytes), destinationStart);
  }
  markDirty(header.x, header.y + header.rowStart, header.width, rows);
  pending.received += 1;
  if (pending.received >= pending.total) {
    if (pending.full && awaitingFull) {
      awaitingFull = false;
      stopFullRetry();
    }
    pending = null;
  }
  scheduleDraw();
}

function markDirty(x: number, y: number, width: number, height: number) {
  if (!dirty) {
    dirty = { x, y, width, height };
    return;
  }
  const left = Math.min(dirty.x, x);
  const top = Math.min(dirty.y, y);
  const right = Math.max(dirty.x + dirty.width, x + width);
  const bottom = Math.max(dirty.y + dirty.height, y + height);
  dirty = { x: left, y: top, width: right - left, height: bottom - top };
}

function scheduleDraw() {
  if (drawScheduled) return;
  drawScheduled = true;
  requestAnimationFrame(() => {
    drawScheduled = false;
    if (!context || !imageData || !dirty) return;
    const region = dirty;
    dirty = null;
    context.putImageData(imageData, 0, 0, region.x, region.y, region.width, region.height);
  });
}

function requestFullWithRetry() {
  if (props.sessionId === null || !awaitingFull || fullRetryTimer !== null) return;
  let attempts = 0;
  const tick = () => {
    fullRetryTimer = null;
    if (!awaitingFull || props.sessionId === null || attempts >= 10) return;
    attempts += 1;
    emit("full-frame-request");
    fullRetryTimer = window.setTimeout(tick, 500);
  };
  tick();
}

function stopFullRetry() {
  if (fullRetryTimer !== null) {
    window.clearTimeout(fullRetryTimer);
    fullRetryTimer = null;
  }
}

function focusInput() {
  inputSink.value?.focus({ preventScroll: true });
}

function onStagePointerDown(event: PointerEvent) {
  if (event.button !== 0) return;
  focusInput();
}

function onFocus() {
  focused.value = true;
  emit("focused", true);
  send(releaseAllPacket(props.sessionId ?? 0));
  sendFocusSync();
}

function onBlur() {
  focused.value = false;
  suppressedZoomKeys.clear();
  emit("focused", false);
  send(releaseAllPacket(props.sessionId ?? 0));
}

function sendFocusSync() {
  if (props.sessionId !== null) {
    sendInput(
      focusSyncPacket(
        props.sessionId,
        modifierState.scrollLock,
        modifierState.numLock,
        modifierState.capsLock,
        modifierState.kanaLock,
      ),
    );
  }
}

function rememberModifiers(event: KeyboardEvent) {
  modifierState.scrollLock = event.getModifierState("ScrollLock");
  modifierState.numLock = event.getModifierState("NumLock");
  modifierState.capsLock = event.getModifierState("CapsLock");
  modifierState.kanaLock = event.getModifierState("KanaMode");
}

function send(packet: Uint8Array) {
  if (props.sessionId === null) return;
  sendInput(packet);
}

function sendKey(scancode: { code: number; extended: boolean }, pressed: boolean) {
  if (props.sessionId === null) return;
  sendInput(keyPacket(props.sessionId, scancode.code, scancode.extended, pressed));
}

function sendUnicode(character: string) {
  if (props.sessionId === null) return;
  const codepoint = character.codePointAt(0);
  if (codepoint === undefined) return;
  sendInput(unicodePacket(props.sessionId, codepoint, true));
  sendInput(unicodePacket(props.sessionId, codepoint, false));
}

function zoomShortcut(event: KeyboardEvent): "in" | "out" | "reset" | null {
  const modifier = isMacPlatform
    ? event.metaKey && !event.ctrlKey
    : event.ctrlKey && !event.metaKey;
  if (!modifier || event.altKey) return null;
  const code = event.code;
  // Cmd+Plus arrives as Cmd+Shift+Equal on macOS.
  if (code === "Equal" || code === "NumpadAdd") return "in";
  if (event.shiftKey) return null;
  if (code === "Minus" || code === "NumpadSubtract") return "out";
  if (code === "Digit0" || code === "Numpad0") return "reset";
  return null;
}

function onKeyDown(event: KeyboardEvent) {
  if (event.isComposing) return;
  rememberModifiers(event);
  const zoom = zoomShortcut(event);
  if (zoom) {
    event.preventDefault();
    suppressedZoomKeys.add(event.code);
    emit("zoom-change", zoom);
    return;
  }
  const pasteCombo = (event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === "v";
  if (isMacPlatform && event.metaKey) {
    if (pasteCombo) {
      // Leave the default action intact so the browser can emit `paste`.
      beginPasteShortcut();
      return;
    }
    const shortcut = remapMacCommand(event, props.remapShortcuts);
    if (shortcut) {
      event.preventDefault();
      const keys = shortcutTapKeys(shortcut);
      for (const key of keys) sendKey(key, true);
      for (const key of [...keys].reverse()) sendKey(key, false);
      return;
    }
    // Cmd combinations without a Windows equivalent stay local; forwarding
    // Cmd itself would tap the remote Start menu.
    event.preventDefault();
    return;
  }
  if (pasteCombo) {
    if (isMacPlatform) {
      // Ctrl+V is not a browser paste shortcut on macOS. Forward it as a real
      // remote Ctrl+V so a Windows app can paste from its own clipboard.
      const scancode = scancodeForCode(event.code);
      if (scancode) {
        event.preventDefault();
        sendKey(scancode, true);
      }
      return;
    }
    // Ctrl+V is left to the browser so a native `paste` event can deliver the
    // clipboard text; the API fallback covers hosts that swallow it.
    beginPasteShortcut();
    interceptedPaste = true;
    return;
  }
  const scancode = scancodeForCode(event.code);
  if (scancode) {
    event.preventDefault();
    sendKey(scancode, true);
    return;
  }
  if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
    event.preventDefault();
    sendUnicode(event.key);
  }
}

function onKeyUp(event: KeyboardEvent) {
  if (event.isComposing) return;
  rememberModifiers(event);
  // Do not forward the release of a key whose press was consumed by local
  // zoom, even if the user releases the modifier first.
  if (suppressedZoomKeys.delete(event.code)) {
    event.preventDefault();
    return;
  }
  if (isMacPlatform && (event.metaKey || event.code === "MetaLeft" || event.code === "MetaRight")) {
    return;
  }
  if (interceptedPaste && event.key.toLowerCase() === "v") {
    interceptedPaste = false;
    event.preventDefault();
    return;
  }
  const scancode = scancodeForCode(event.code);
  if (scancode) {
    event.preventDefault();
    sendKey(scancode, false);
  }
}

function onCompositionEnd(event: CompositionEvent) {
  const text = event.data || inputSink.value?.value || "";
  if (inputSink.value) inputSink.value.value = "";
  for (const character of text) {
    sendUnicode(character);
  }
}

function queuePaste(text: string) {
  if (!text) return;
  const now = Date.now();
  if (text === lastPaste.text && now - lastPaste.at < PASTE_DEDUPE_MS) return;
  lastPaste = { text, at: now };
  emit("paste-text", text);
}

function beginPasteShortcut() {
  const token = ++pasteToken;
  if (pasteFallbackTimer !== null) window.clearTimeout(pasteFallbackTimer);
  pasteFallbackTimer = window.setTimeout(() => {
    pasteFallbackTimer = null;
    if (token !== pasteToken) return;
    void readClipboard().then((result) => {
      if (token !== pasteToken) return;
      if (!result.ok) {
        emit("paste-error", result.reason);
        return;
      }
      queuePaste(result.text);
    });
  }, PASTE_FALLBACK_MS);
}

function cancelPasteFallback() {
  pasteToken += 1;
  if (pasteFallbackTimer !== null) {
    window.clearTimeout(pasteFallbackTimer);
    pasteFallbackTimer = null;
  }
}

function onPaste(event: ClipboardEvent) {
  const data = event.clipboardData;
  const text = data?.getData("text/plain") ?? "";
  let imageFile: File | null = null;
  if (data?.items) {
    for (const item of Array.from(data.items)) {
      if (item.kind === "file" && item.type.startsWith("image/")) {
        imageFile = item.getAsFile();
        if (imageFile) break;
      }
    }
  }
  if (inputSink.value) inputSink.value.value = "";
  if (!text && !imageFile) return;
  event.preventDefault();
  cancelPasteFallback();
  if (imageFile) {
    const file = imageFile;
    void imageToPng(file)
      .then((bytes) => {
        emit("paste-content", {
          text: text || undefined,
          image: { bytes, mime: "image/png", name: file.name || "clipboard.png" },
        });
      })
      .catch(() => emit("paste-error", "invalid-image"));
    return;
  }
  queuePaste(text);
}

/** The sidecar expects PNG; WebKit may hand over TIFF or JPEG images. */
async function imageToPng(file: File): Promise<ArrayBuffer> {
  if (file.type === "image/png") return await file.arrayBuffer();
  const bitmap = await createImageBitmap(file);
  try {
    const canvas = document.createElement("canvas");
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("Cannot encode the clipboard image");
    context.drawImage(bitmap, 0, 0);
    const blob = await new Promise<Blob | null>((resolve) => {
      canvas.toBlob(resolve, "image/png");
    });
    if (!blob) throw new Error("Cannot encode the clipboard image");
    return await blob.arrayBuffer();
  } finally {
    bitmap.close?.();
  }
}

function remoteCoordinates(event: MouseEvent): { x: number; y: number } {
  const element = canvas.value;
  if (!element) return { x: 0, y: 0 };
  const rect = element.getBoundingClientRect();
  const scaleX = rect.width > 0 ? props.desktopWidth / rect.width : 1;
  const scaleY = rect.height > 0 ? props.desktopHeight / rect.height : 1;
  const x = Math.max(0, Math.min(props.desktopWidth - 1, Math.round((event.clientX - rect.left) * scaleX)));
  const y = Math.max(0, Math.min(props.desktopHeight - 1, Math.round((event.clientY - rect.top) * scaleY)));
  return { x, y };
}

function onMouseMove(event: MouseEvent) {
  const position = remoteCoordinates(event);
  lastRemote = position;
  pendingMouse = position;
  if (mouseFrame !== null) return;
  mouseFrame = requestAnimationFrame(() => {
    mouseFrame = null;
    const pendingPosition = pendingMouse;
    pendingMouse = null;
    if (!pendingPosition || props.sessionId === null) return;
    sendInput(mouseMovePacket(props.sessionId, pendingPosition.x, pendingPosition.y));
  });
}

function onMouseDown(event: MouseEvent) {
  event.preventDefault();
  // Only the left button moves keyboard focus; focusing on right click is what
  // made the native context menu fight the remote one.
  if (event.button === 0) focusInput();
  const button = rdpButton(event.button);
  if (button === null || props.sessionId === null) return;
  const position = remoteCoordinates(event);
  lastRemote = position;
  pressedButtons.add(event.button);
  sendInput(mouseButtonPacket(props.sessionId, button, true, position.x, position.y));
}

function onMouseUp(event: MouseEvent) {
  const position = remoteCoordinates(event);
  lastRemote = position;
  const button = rdpButton(event.button);
  if (button === null || props.sessionId === null) return;
  // Only release buttons the remote actually saw pressed. The browser also
  // emits a mouseup for the suppressed native context menu.
  if (!pressedButtons.delete(event.button)) return;
  sendInput(mouseButtonPacket(props.sessionId, button, false, position.x, position.y));
}

function onContextMenu(event: MouseEvent) {
  event.preventDefault();
  // Native menus can swallow the following mouseup and leave a remote button
  // held down, which shows a persistent translucent selection overlay.
  releasePressedButtons();
}

function onGlobalMouseUp(event: MouseEvent) {
  // Release buttons whose mouseup happened outside the canvas (toolbar, native
  // menus, window edges) so the remote never keeps a button stuck.
  if (!pressedButtons.has(event.button)) return;
  const button = rdpButton(event.button);
  pressedButtons.delete(event.button);
  if (button === null || props.sessionId === null) return;
  sendInput(mouseButtonPacket(props.sessionId, button, false, lastRemote.x, lastRemote.y));
}

function releasePressedButtons() {
  if (props.sessionId === null || pressedButtons.size === 0) return;
  for (const pressed of pressedButtons) {
    const button = rdpButton(pressed);
    if (button !== null) {
      sendInput(mouseButtonPacket(props.sessionId, button, false, lastRemote.x, lastRemote.y));
    }
  }
  pressedButtons.clear();
}

function onWheel(event: WheelEvent) {
  event.preventDefault();
  if (props.sessionId === null) return;
  const { vertical, delta } = wheelUnits(event.deltaY, event.deltaX, event.deltaMode);
  if (delta === 0) return;
  const position = remoteCoordinates(event);
  // Trackpads emit dozens of tiny deltas per gesture. Sum them per animation
  // frame so the remote receives one smooth packet instead of overshooting.
  if (pendingWheel.delta !== 0 && pendingWheel.vertical !== vertical) {
    flushWheel();
  }
  pendingWheel.vertical = vertical;
  pendingWheel.delta = Math.max(-32767, Math.min(32767, pendingWheel.delta + delta));
  pendingWheel.x = position.x;
  pendingWheel.y = position.y;
  if (wheelFrame === null) {
    wheelFrame = requestAnimationFrame(() => {
      wheelFrame = null;
      flushWheel();
    });
  }
}

function flushWheel() {
  if (wheelFrame !== null) {
    cancelAnimationFrame(wheelFrame);
    wheelFrame = null;
  }
  const { vertical, delta, x, y } = pendingWheel;
  pendingWheel.delta = 0;
  if (delta === 0 || props.sessionId === null) return;
  sendInput(wheelPacket(props.sessionId, vertical, delta, x, y));
}

function sendCtrlAltDel() {
  focusInput();
  sendKey(CTRL, true);
  sendKey(ALT, true);
  sendKey(DELETE_KEY, true);
  sendKey(DELETE_KEY, false);
  sendKey(ALT, false);
  sendKey(CTRL, false);
}

function sendCtrlV() {
  focusInput();
  sendKey(CTRL, true);
  sendKey(V_KEY, true);
  sendKey(V_KEY, false);
  sendKey(CTRL, false);
}

function sendMeta() {
  focusInput();
  sendKey(META, true);
  sendKey(META, false);
}

function sendEscape() {
  focusInput();
  sendKey({ code: 0x01, extended: false }, true);
  sendKey({ code: 0x01, extended: false }, false);
}

defineExpose({
  handleBinary,
  sendCtrlAltDel,
  sendCtrlV,
  sendMeta,
  sendEscape,
  focusInput,
});
</script>

<template>
  <div
    ref="stage"
    class="rdp-stage"
    :class="{ 'rdp-stage--manual': manualZoom }"
    @pointerdown="onStagePointerDown"
    @contextmenu="onContextMenu"
  >
    <div class="rdp-scroll">
      <canvas
        ref="canvas"
        class="rdp-canvas"
        :style="canvasStyle"
        tabindex="0"
        @mousemove="onMouseMove"
        @mousedown="onMouseDown"
        @mouseup="onMouseUp"
        @wheel.prevent="onWheel"
        @contextmenu="onContextMenu"
      />
    </div>
    <textarea
      ref="inputSink"
      class="rdp-input-sink"
      autocomplete="off"
      autocorrect="off"
      autocapitalize="off"
      spellcheck="false"
      aria-label="Remote desktop keyboard input"
      @focus="onFocus"
      @blur="onBlur"
      @keydown="onKeyDown"
      @keyup="onKeyUp"
      @paste="onPaste"
      @compositionend="onCompositionEnd"
    />
  </div>
</template>

<style scoped>
.rdp-stage {
  position: relative;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 100%;
  height: 100%;
  min-width: 0;
  min-height: 0;
  overflow: hidden;
  background: #000;
  outline: none;
  user-select: none;
  -webkit-user-select: none;
  touch-action: none;
  -webkit-tap-highlight-color: transparent;
}

.rdp-stage--manual {
  display: block;
  overflow: auto;
}

.rdp-scroll {
  display: flex;
  align-items: center;
  justify-content: center;
  min-width: 100%;
  min-height: 100%;
}

.rdp-stage--manual .rdp-scroll {
  width: max-content;
  height: max-content;
}

.rdp-canvas {
  display: block;
  flex: 0 0 auto;
  max-width: 100%;
  max-height: 100%;
  background: #000;
  outline: none;
  image-rendering: auto;
  cursor: default;
  touch-action: none;
  -webkit-user-drag: none;
}

.rdp-stage--manual .rdp-canvas {
  max-width: none;
  max-height: none;
}

.rdp-input-sink {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  padding: 0;
  border: 0;
  opacity: 0;
  resize: none;
  pointer-events: none;
  background: transparent;
  color: transparent;
  caret-color: transparent;
  outline: none;
  user-select: none;
  -webkit-user-select: none;
}
</style>
