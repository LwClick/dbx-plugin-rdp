import { api } from "./bridge";

export const CLIPBOARD_CHANNEL = "rdp-clipboard-v1";
export const CLIPBOARD_HEADER_LEN = 20;
export const CLIPBOARD_CHUNK_BYTES = 1024 * 1024;
export const MAX_CLIPBOARD_IMAGE_BYTES = 24 * 1024 * 1024;

const CLIPBOARD_MAGIC = 0xc2;
const CLIPBOARD_VERSION = 1;
const CLIPBOARD_TYPE_IMAGE = 1;

export interface ClipboardImageEvent {
  connectionId: string;
  sessionId?: number;
  token: string;
  name: string;
  size: number;
  width: number;
  height: number;
}

export interface PasteContent {
  text?: string;
  image?: { bytes: ArrayBuffer; mime: string; name: string };
}

/** Builds one `rdp-clipboard-v1` image chunk packet. */
export function imageChunkPacket(
  sessionId: number,
  token: number,
  chunkIndex: number,
  chunkCount: number,
  totalLen: number,
  payload: Uint8Array,
): Uint8Array {
  const bytes = new Uint8Array(CLIPBOARD_HEADER_LEN + payload.length);
  const view = new DataView(bytes.buffer);
  view.setUint8(0, CLIPBOARD_MAGIC);
  view.setUint8(1, CLIPBOARD_VERSION);
  view.setUint8(2, CLIPBOARD_TYPE_IMAGE);
  view.setUint8(3, 0);
  view.setUint32(4, token >>> 0, false);
  view.setUint32(8, sessionId >>> 0, false);
  view.setUint16(12, chunkIndex, false);
  view.setUint16(14, chunkCount, false);
  view.setUint32(16, totalLen, false);
  bytes.set(payload, CLIPBOARD_HEADER_LEN);
  return bytes;
}

export function createImageToken(): number {
  const generator = typeof crypto !== "undefined" ? crypto : undefined;
  if (generator && typeof generator.getRandomValues === "function") {
    const buffer = new Uint32Array(1);
    generator.getRandomValues(buffer);
    return buffer[0] || 1;
  }
  return (Math.floor(Math.random() * 0xfffffffe) + 1) >>> 0;
}

/**
 * Uploads a PNG in 1 MiB chunks, then asks the sidecar to put it on the remote
 * clipboard together with any text that was copied alongside the image.
 */
export async function pasteImageToRemote(
  connectionId: string,
  sessionId: number,
  bytes: Uint8Array,
  text: string,
): Promise<void> {
  if (bytes.length === 0) throw new Error("The clipboard image is empty");
  if (bytes.length > MAX_CLIPBOARD_IMAGE_BYTES) {
    throw new Error("The clipboard image is larger than 24 MiB");
  }
  const token = createImageToken();
  const count = Math.ceil(bytes.length / CLIPBOARD_CHUNK_BYTES);
  for (let index = 0; index < count; index += 1) {
    const start = index * CLIPBOARD_CHUNK_BYTES;
    const payload = bytes.subarray(
      start,
      Math.min(start + CLIPBOARD_CHUNK_BYTES, bytes.length),
    );
    await api().sendBinary(
      CLIPBOARD_CHANNEL,
      imageChunkPacket(sessionId, token, index, count, bytes.length, payload),
    );
  }
  await api().invoke("rdp/clipboard/content/paste", {
    connectionId,
    token,
    text,
  });
}

/** Mirrors `saveRemoteFile` for the PNG staged from the remote clipboard. */
export async function saveRemoteImage(
  connectionId: string,
  token: string,
  name: string,
  size: number,
): Promise<boolean> {
  const transfer = api().fileTransfer;
  if (!transfer) throw new Error("This DBX host does not support file transfer");
  const target = await transfer.beginSave({ name, size, contentType: "image/png" });
  if (!target) {
    await api().invoke("rdp/clipboard/image/release", { connectionId, token }).catch(() => undefined);
    return false;
  }
  let offset = 0;
  try {
    for (;;) {
      const chunk = await api().invoke<{ dataBase64: string; length: number; eof: boolean }>(
        "rdp/clipboard/image/read",
        { connectionId, token, offset, length: CLIPBOARD_CHUNK_BYTES },
      );
      if (chunk.length > 0) {
        const written = await transfer.write(target.handleId, offset, chunk.dataBase64);
        offset =
          typeof written.nextOffset === "number" ? written.nextOffset : offset + chunk.length;
      }
      if (chunk.eof) break;
      if (chunk.length === 0) throw new Error("Empty image chunk");
    }
    await transfer.finish(target.handleId);
    return true;
  } catch (failure) {
    await transfer.cancel(target.handleId).catch(() => undefined);
    throw failure;
  } finally {
    await api().invoke("rdp/clipboard/image/release", { connectionId, token }).catch(
      () => undefined,
    );
  }
}

/** Reads a PNG from the host clipboard; returns null when unavailable or empty. */
export async function readClipboardImage(): Promise<Uint8Array | null> {
  const capabilities = api().capabilities ?? {};
  const clipboard = api().clipboard;
  if (
    capabilities.clipboardImageRead === false ||
    !clipboard ||
    typeof clipboard.readImage !== "function"
  ) {
    return null;
  }
  try {
    const base64 = await clipboard.readImage();
    if (!base64) return null;
    const binary = atob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) {
      bytes[index] = binary.charCodeAt(index);
    }
    return bytes;
  } catch {
    return null;
  }
}

/** Debounced local clipboard broadcast used when the workbench regains focus. */
export function createClipboardAnnouncer(
  readText: () => Promise<string | null>,
  advertise: (text: string) => Promise<unknown>,
): (force?: boolean) => void {
  let last = 0;
  let lastText = "";
  return (force = false) => {
    const now = Date.now();
    // The host rate-limits clipboard reads (1/s) and audits them per session.
    // Focus-driven broadcasts stay sparse; a forced call runs on every new
    // session so the remote gets the current text even when it did not change.
    if (!force && now - last < 3000) return;
    last = now;
    void readText()
      .then((text) => {
        if (!text) return;
        if (!force && text === lastText) return;
        lastText = text;
        return advertise(text);
      })
      .catch(() => undefined);
  };
}
