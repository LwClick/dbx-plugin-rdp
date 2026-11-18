export const CURSOR_CHANNEL = "rdp-cursor-v1";

export const CURSOR_MAGIC = 0xd3;
export const CURSOR_PROTOCOL_VERSION = 1;
export const CURSOR_HEADER_LEN = 16;

export const CURSOR_KIND_DEFAULT = 0;
export const CURSOR_KIND_HIDDEN = 1;
export const CURSOR_KIND_BITMAP = 2;

export type CursorKind = "default" | "hidden" | "bitmap";

export interface CursorMessage {
  sessionId: number;
  kind: CursorKind;
  hotspotX: number;
  hotspotY: number;
  width: number;
  height: number;
  bitmap?: Uint8Array;
}

export class CursorProtocolError extends Error {}

export function parseCursorMessage(data: Uint8Array): CursorMessage {
  if (data.length < CURSOR_HEADER_LEN) {
    throw new CursorProtocolError("RDP cursor is truncated");
  }
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  if (view.getUint8(0) !== CURSOR_MAGIC) {
    throw new CursorProtocolError("RDP cursor has a bad magic byte");
  }
  if (view.getUint8(1) !== CURSOR_PROTOCOL_VERSION) {
    throw new CursorProtocolError("RDP cursor uses an unsupported protocol version");
  }
  const rawKind = view.getUint8(2);
  const width = view.getUint16(12, false);
  const height = view.getUint16(14, false);
  const base: CursorMessage = {
    sessionId: view.getUint32(4, false),
    kind: "default",
    hotspotX: view.getUint16(8, false),
    hotspotY: view.getUint16(10, false),
    width,
    height,
  };
  if (rawKind === CURSOR_KIND_DEFAULT) return base;
  if (rawKind === CURSOR_KIND_HIDDEN) return { ...base, kind: "hidden" };
  if (rawKind !== CURSOR_KIND_BITMAP) {
    throw new CursorProtocolError(`RDP cursor has an unknown kind ${rawKind}`);
  }
  const expected = width * height * 4;
  const bitmap = data.subarray(CURSOR_HEADER_LEN);
  if (expected === 0 || bitmap.length !== expected) {
    throw new CursorProtocolError(`RDP cursor bitmap is ${bitmap.length} bytes, expected ${expected}`);
  }
  return { ...base, kind: "bitmap", bitmap };
}
