export const INPUT_MAGIC = 0xd2;
export const INPUT_VERSION = 1;

export const INPUT_TYPE_KEY = 1;
export const INPUT_TYPE_MOUSE_MOVE = 2;
export const INPUT_TYPE_MOUSE_BUTTON = 3;
export const INPUT_TYPE_WHEEL = 4;
export const INPUT_TYPE_UNICODE = 5;
export const INPUT_TYPE_RELEASE_ALL = 6;
export const INPUT_TYPE_FOCUS_SYNC = 7;

export const INPUT_HEADER_LEN = 8;

function header(length: number, type: number, sessionId: number): { bytes: Uint8Array; view: DataView } {
  const bytes = new Uint8Array(length);
  const view = new DataView(bytes.buffer);
  view.setUint8(0, INPUT_MAGIC);
  view.setUint8(1, INPUT_VERSION);
  view.setUint8(2, type);
  view.setUint8(3, 0);
  view.setUint32(4, sessionId, false);
  return { bytes, view };
}

export function keyPacket(sessionId: number, scancode: number, extended: boolean, pressed: boolean): Uint8Array {
  const { bytes, view } = header(INPUT_HEADER_LEN + 3, INPUT_TYPE_KEY, sessionId);
  view.setUint8(INPUT_HEADER_LEN, scancode & 0xff);
  view.setUint8(INPUT_HEADER_LEN + 1, extended ? 1 : 0);
  view.setUint8(INPUT_HEADER_LEN + 2, pressed ? 1 : 0);
  return bytes;
}

export function mouseMovePacket(sessionId: number, x: number, y: number): Uint8Array {
  const { bytes, view } = header(INPUT_HEADER_LEN + 4, INPUT_TYPE_MOUSE_MOVE, sessionId);
  view.setUint16(INPUT_HEADER_LEN, clampU16(x), false);
  view.setUint16(INPUT_HEADER_LEN + 2, clampU16(y), false);
  return bytes;
}

export function mouseButtonPacket(sessionId: number, button: number, pressed: boolean, x: number, y: number): Uint8Array {
  const { bytes, view } = header(INPUT_HEADER_LEN + 6, INPUT_TYPE_MOUSE_BUTTON, sessionId);
  view.setUint8(INPUT_HEADER_LEN, button);
  view.setUint8(INPUT_HEADER_LEN + 1, pressed ? 1 : 0);
  view.setUint16(INPUT_HEADER_LEN + 2, clampU16(x), false);
  view.setUint16(INPUT_HEADER_LEN + 4, clampU16(y), false);
  return bytes;
}

export function wheelPacket(sessionId: number, vertical: boolean, delta: number, x: number, y: number): Uint8Array {
  const { bytes, view } = header(INPUT_HEADER_LEN + 7, INPUT_TYPE_WHEEL, sessionId);
  view.setUint8(INPUT_HEADER_LEN, vertical ? 1 : 0);
  view.setInt16(INPUT_HEADER_LEN + 1, clampI16(delta), false);
  view.setUint16(INPUT_HEADER_LEN + 3, clampU16(x), false);
  view.setUint16(INPUT_HEADER_LEN + 5, clampU16(y), false);
  return bytes;
}

export function unicodePacket(sessionId: number, codepoint: number, pressed: boolean): Uint8Array {
  const { bytes, view } = header(INPUT_HEADER_LEN + 5, INPUT_TYPE_UNICODE, sessionId);
  view.setUint32(INPUT_HEADER_LEN, codepoint >>> 0, false);
  view.setUint8(INPUT_HEADER_LEN + 4, pressed ? 1 : 0);
  return bytes;
}

export function releaseAllPacket(sessionId: number): Uint8Array {
  const { bytes } = header(INPUT_HEADER_LEN, INPUT_TYPE_RELEASE_ALL, sessionId);
  return bytes;
}

export function focusSyncPacket(
  sessionId: number,
  scrollLock: boolean,
  numLock: boolean,
  capsLock: boolean,
  kanaLock = false,
): Uint8Array {
  const { bytes, view } = header(INPUT_HEADER_LEN + 4, INPUT_TYPE_FOCUS_SYNC, sessionId);
  view.setUint8(INPUT_HEADER_LEN, scrollLock ? 1 : 0);
  view.setUint8(INPUT_HEADER_LEN + 1, numLock ? 1 : 0);
  view.setUint8(INPUT_HEADER_LEN + 2, capsLock ? 1 : 0);
  view.setUint8(INPUT_HEADER_LEN + 3, kanaLock ? 1 : 0);
  return bytes;
}

/** Browser button index -> RDP button index (0 left, 1 middle, 2 right, 3 X1, 4 X2). */
export function rdpButton(button: number): number | null {
  switch (button) {
    case 0:
      return 0;
    case 1:
      return 1;
    case 2:
      return 2;
    case 3:
      return 3;
    case 4:
      return 4;
    default:
      return null;
  }
}

/** Wheel notch in RDP rotation units; browser deltaY is positive downwards. */
export function wheelDelta(deltaY: number, deltaX = 0): { vertical: boolean; delta: number } {
  const vertical = Math.abs(deltaY) >= Math.abs(deltaX);
  const raw = vertical ? deltaY : deltaX;
  if (!Number.isFinite(raw) || raw === 0) {
    return { vertical, delta: 0 };
  }
  const units = Math.sign(raw) * 120;
  return { vertical, delta: -units };
}

function clampU16(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(65535, Math.round(value)));
}

function clampI16(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(-32768, Math.min(32767, Math.round(value)));
}
