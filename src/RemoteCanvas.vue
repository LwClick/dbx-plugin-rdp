<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

import {
  FRAME_TYPE_BITMAP,
  FRAME_TYPE_RESIZE,
  isFullDesktopFrame,
  splitFrame,
} from "./frameProtocol";
import {
  focusSyncPacket,
  keyPacket,
  mouseButtonPacket,
  mouseMovePacket,
  rdpButton,
  releaseAllPacket,
  unicodePacket,
  wheelDelta,
  wheelPacket,
} from "./input";
import { sendInput } from "./bridge";
import { ALT, CTRL, DELETE_KEY, META, V_KEY, scancodeForCode } from "./scancodes";

const props = defineProps<{
  sessionId: number | null;
  desktopWidth: number;
  desktopHeight: number;
}>();

const emit = defineEmits<{
  (event: "desktop-size", width: number, height: number): void;
  (event: "resize-request", width: number, height: number): void;
  (event: "full-frame-request"): void;
  (event: "paste-request"): void;
  (event: "focused", value: boolean): void;
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
let lastFullRequest = 0;
let resizeTimer: number | null = null;
let mouseFrame: number | null = null;
let pendingMouse: { x: number; y: number } | null = null;
let interceptedPaste = false;
let resizeObserver: ResizeObserver | null = null;

const focused = ref(false);
const stageWidth = ref(0);
const stageHeight = ref(0);
const surfaceVersion = ref(0);
const modifierState = { scrollLock: false, numLock: false, capsLock: false, kanaLock: false };

const canvasStyle = computed(() => {
  // `surfaceVersion` makes this computed re-run when the remote framebuffer is
  // recreated even though the pixel dimensions are plain variables.
  void surfaceVersion.value;
  if (surfaceWidth === 0 || surfaceHeight === 0) return {};
  const availableWidth = stageWidth.value || surfaceWidth;
  const availableHeight = stageHeight.value || surfaceHeight;
  const scale = Math.min(availableWidth / surfaceWidth, availableHeight / surfaceHeight, 1);
  return {
    width: `${Math.max(1, Math.floor(surfaceWidth * scale))}px`,
    height: `${Math.max(1, Math.floor(surfaceHeight * scale))}px`,
  };
});

onMounted(() => {
  resetSurface(props.desktopWidth, props.desktopHeight);
  resizeObserver = new ResizeObserver(() => {
    fitToStage();
    scheduleRemoteResize();
  });
  if (stage.value) resizeObserver.observe(stage.value);
  window.addEventListener("resize", fitToStage);
});

onBeforeUnmount(() => {
  resizeObserver?.disconnect();
  resizeObserver = null;
  window.removeEventListener("resize", fitToStage);
  if (resizeTimer !== null) window.clearTimeout(resizeTimer);
  if (mouseFrame !== null) cancelAnimationFrame(mouseFrame);
});

watch(
  () => [props.desktopWidth, props.desktopHeight] as const,
  ([width, height]) => {
    if (width === surfaceWidth && height === surfaceHeight) return;
    resetSurface(width, height);
  },
);

function resetSurface(width: number, height: number) {
  const element = canvas.value;
  if (!element || width <= 0 || height <= 0) return;
  surfaceWidth = width;
  surfaceHeight = height;
  element.width = width;
  element.height = height;
  context = element.getContext("2d", { alpha: false });
  if (!context) return;
  context.imageSmoothingEnabled = false;
  imageData = context.createImageData(width, height);
  for (let index = 3; index < imageData.data.length; index += 4) {
    imageData.data[index] = 255;
  }
  context.fillStyle = "#000";
  context.fillRect(0, 0, width, height);
  dirty = null;
  pending = null;
  awaitingFull = true;
  surfaceVersion.value += 1;
  fitToStage();
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
    if (!element || props.sessionId === null) return;
    const availableWidth = element.clientWidth;
    const availableHeight = element.clientHeight;
    if (availableWidth < 320 || availableHeight < 240) return;
    const width = Math.max(1024, Math.min(2560, Math.round(availableWidth)));
    const height = Math.max(600, Math.min(1600, Math.round((width * availableHeight) / availableWidth)));
    if (Math.abs(width - props.desktopWidth) > 24 || Math.abs(height - props.desktopHeight) > 24) {
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
    resetSurface(header.desktopWidth, header.desktopHeight);
    emit("desktop-size", header.desktopWidth, header.desktopHeight);
    return;
  }
  if (header.frameType !== FRAME_TYPE_BITMAP || header.chunkCount === 0) return;

  const full = isFullDesktopFrame(header);
  if (pending && header.seq !== pending.seq) {
    if (pending.received < pending.total) {
      requestFull();
      pending = null;
      awaitingFull = true;
      return;
    }
    pending = null;
  }
  if (awaitingFull && !full) {
    requestFull();
    return;
  }
  if (!pending) {
    pending = { seq: header.seq, total: header.chunkCount, received: 0, full };
  }
  if (header.chunkIndex !== pending.received) {
    requestFull();
    pending = null;
    awaitingFull = true;
    return;
  }
  if (!imageData || surfaceWidth !== header.desktopWidth || surfaceHeight !== header.desktopHeight) {
    return;
  }

  const rows = header.height - header.rowStart;
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

function requestFull() {
  const now = performance.now();
  if (now - lastFullRequest < 250) return;
  lastFullRequest = now;
  emit("full-frame-request");
}

function focusInput() {
  inputSink.value?.focus({ preventScroll: true });
}

function onFocus() {
  focused.value = true;
  emit("focused", true);
  send(releaseAllPacket(props.sessionId ?? 0));
  sendFocusSync();
}

function onBlur() {
  focused.value = false;
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

function onKeyDown(event: KeyboardEvent) {
  if (event.isComposing) return;
  rememberModifiers(event);
  if ((event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === "v" && event.shiftKey) {
    event.preventDefault();
    event.stopPropagation();
    emit("paste-request");
    interceptedPaste = true;
    return;
  }
  if (event.ctrlKey && !event.altKey && !event.metaKey && event.key.toLowerCase() === "v") {
    event.preventDefault();
    event.stopPropagation();
    emit("paste-request");
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
  pendingMouse = remoteCoordinates(event);
  if (mouseFrame !== null) return;
  mouseFrame = requestAnimationFrame(() => {
    mouseFrame = null;
    const position = pendingMouse;
    pendingMouse = null;
    if (!position || props.sessionId === null) return;
    sendInput(mouseMovePacket(props.sessionId, position.x, position.y));
  });
}

function onMouseDown(event: MouseEvent) {
  event.preventDefault();
  focusInput();
  const button = rdpButton(event.button);
  if (button === null || props.sessionId === null) return;
  const position = remoteCoordinates(event);
  sendInput(mouseButtonPacket(props.sessionId, button, true, position.x, position.y));
}

function onMouseUp(event: MouseEvent) {
  const button = rdpButton(event.button);
  if (button === null || props.sessionId === null) return;
  const position = remoteCoordinates(event);
  sendInput(mouseButtonPacket(props.sessionId, button, false, position.x, position.y));
}

function onWheel(event: WheelEvent) {
  event.preventDefault();
  if (props.sessionId === null) return;
  const { vertical, delta } = wheelDelta(event.deltaY, event.deltaX);
  if (delta === 0) return;
  const position = remoteCoordinates(event);
  sendInput(wheelPacket(props.sessionId, vertical, delta, position.x, position.y));
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
  <div ref="stage" class="rdp-stage" @pointerdown="focusInput">
    <canvas
      ref="canvas"
      class="rdp-canvas"
      :class="{ 'rdp-canvas--focused': focused }"
      :style="canvasStyle"
      tabindex="0"
      @mousemove="onMouseMove"
      @mousedown="onMouseDown"
      @mouseup="onMouseUp"
      @wheel.prevent="onWheel"
      @contextmenu.prevent
    />
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
}

.rdp-canvas {
  display: block;
  max-width: 100%;
  max-height: 100%;
  background: #000;
  outline: none;
  image-rendering: auto;
  cursor: default;
}

.rdp-canvas--focused {
  cursor: none;
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
}
</style>
